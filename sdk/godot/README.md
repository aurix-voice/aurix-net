# Aurix Net for Godot 4 (GDExtension)

Build: `git clone -b 4.2 https://github.com/godotengine/godot-cpp` here, `cargo build --release -p net-ffi`
in the repo root, then `scons target=template_debug` (`SConstruct` builds godot-cpp with exceptions on:
`aurix_net.hpp` reports errors by throwing, the wrappers catch them). Copy `bin/` + `aurix_net.gdextension` to
`addons/aurix_net/` together with `libaurix_net.so` / `aurix_net.dll`.

Smoke test without the editor: `check~/` is a minimal project (classes load, sync schema parses, two relay
clients exchange an event, two more replicate a `Transform` through `AurixNetSync` with 0 gaps, then run a
lockstep room; the `event` signal stays quiet in both).
`cp -r bin ../../target/release/libaurix_net.so check~/addons/aurix_net/bin/`, then
`GRANT_A=... GRANT_B=... GRANT_C=... GRANT_D=... godot --headless --path check~` (grants from
`POST /v1/grants`; without them only the offline part runs; with `NET_SLOW_HTTP`, `GRANT_E` and
`GRANT_F` it also leaves a room and joins another one over a slow link, see `sdk/cpp/README.md`; with
two nodes (`scripts/two-nodes.sh`), `NET_REHOST_HTTP` for the second one, `GRANT_G`, `GRANT_H` and
`NET_REHOST_ROOM` it asks the second node for a room on the first, then waits for the first node to
stop and joins the room again through the second). Godot discovers `.gdextension` files when the
project is opened in the editor; headless, `check~/.godot/extension_list.cfg` lists it by hand.

```gdscript
var net := AurixNetClient.new(); add_child(net)   # polls in _process
net.event.connect(func(code, from, payload): print(code, from, payload))
net.connect_to("https://<node>", grant_from_backend)
net.join_or_create("lobby-1", AurixNetClient.MODE_RELAY, 8)
net.send_event(1, "hi".to_utf8_buffer()); net.flush()
```

A call that fails returns `false` or `-1`; `get_last_error()` has the text and `get_last_error_code()`
the reason as one of `AurixNetClient.ERROR_*` (`ERROR_NOT_FOUND`, `ERROR_ROOM_FULL`, ...).

## Signals added for operator messages

- `notice(text: String, level: String)`: system message pushed by the operator console or the game backend (`level` is `info` or `warn`). Show it in the HUD; it is not a chat message.
- `rehost()`: the room moved to another node during a rolling deploy. Come back with the same grant: `disconnect_from()`, `connect_to()` (the same URL, or another node from `/v1/regions` when it no longer answers), then `join()` the same room. Your actor id is kept.

The C++ header exposes the same as `onNotice` / `onRehost` callbacks.

## aurix-sim

`libaurix_net` also carries the deterministic simulation (`src/aurix_sim.h`: 2D/3D physics, navmesh,
crowd, `sim3_*` convex hulls and casts). Call it from your GDExtension code the same way the client
header is used; values are raw Q32.32 (`aurix_fx_from_double`), handles/ids match the Web and Unity
wrappers, and `sim_golden_hash()` etc. let you assert the build is bit-identical to the other SDKs.

## Sync rooms

```gdscript
var client := AurixNetClient.new(); add_child(client)
var world := AurixNetSync.new()
world.setup(SCHEMA)                       # same IDL text as the room's `schema` prop
client.data.connect(func(_ch, _tick, payload): world.receive(client, payload))
world.spawn.connect(func(entity, owner): print("spawned ", entity, " of ", owner))
world.change.connect(func(entity, _owner): var p: Vector3 = world.get_field(entity, "Transform", "pos"))
func _process(dt):
    world.advance(dt)                     # render tick, 2 ticks behind the server
    world.begin_write("Transform"); world.write_field("pos", position); world.end_write(); world.send_writes(client)
    # more entities you own: queue writes, then var tag := world.spawn_entity(client) (the entity comes in spawn_answer); world.write_entity(client, id); world.despawn_entity(client, id); world.transfer_entity(client, id, other_actor)
```
`send_input(client, bytes)` is the raw input for rooms with server-side `logic`. The room takes one
input per tick, so `send_writes` and `send_input` can be called every frame: the newest of a tick is
the one sent.

## Lockstep rooms

`AurixNetLockstep` is the client side of a lockstep room: the game owns the deterministic
simulation (use `AurixSim` worlds or your own fixed-point code), steps it on every confirmed tick and reports a
hash; the cell compares hashes, serves late joiners from the master's checkpoints and keeps the input journal
for replays.

```gdscript
var ls := AurixNetLockstep.new()
client.data.connect(func(_channel, _tick, payload): ls.receive(client, payload))
ls.tick.connect(func(tick: int, inputs: Array):
    for i in inputs: sim.apply_input(i["actor"], i["data"])   # same order on every peer
    sim.step()
    ls.report_hash(client, tick, sim.hash()))
ls.command.connect(func(tick, actor, data): game.on_command(actor, data))     # SendCommand from any peer
ls.restore.connect(func(tick, hash, state): sim.restore(state))               # on join / resync
ls.checkpoint_requested.connect(func(_tick): ls.send_checkpoint(client, ls.get_confirmed_tick(), sim.hash(), sim.snapshot()))
ls.dump_requested.connect(func(tick): ls.send_dump(client, tick, sim.hash(), sim.snapshot()))
func _physics_process(_dt): ls.send_input(client, encode_local_input())
```

`restore` fires on every join. In a room that has no checkpoint yet it carries tick 0 and an empty
`state`: start from your initial state. Label a checkpoint with `get_confirmed_tick()`, the tick your
simulation runs next, not with the tick in the request: the room can be a tick ahead of you, and a
late joiner restored from a mislabelled state desyncs. `send_input` returns the tick the input is
stamped with, or -1 when this room tick already has your input (the room takes one per tick), so it
is safe to call every frame.

`send_command(client, bytes)` is a reliable message outside the input stream (buy, surrender, pause): the cell
stamps it onto the next tick and every peer gets `command` right before that tick. `set_input_delay(d, min, max,
adaptive)` / `set_input_redundancy(n)` tune latency vs. loss; `get_confirmed_tick()`, `get_server_tick()`,
`get_last_margin()` and `get_late_inputs()` are the live statistics.
