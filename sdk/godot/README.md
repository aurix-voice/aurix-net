# Aurix Net for Godot 4 (GDExtension)

Build: `git clone -b 4.2 https://github.com/godotengine/godot-cpp` here, `cargo build --release -p net-ffi`
in the repo root, then `scons target=template_debug`. Copy `bin/` + `aurix_net.gdextension` to
`addons/aurix_net/` together with `libaurix_net.so` / `aurix_net.dll`.

```gdscript
var net := AurixNetClient.new(); add_child(net)   # polls in _process
net.event.connect(func(code, from, payload): print(code, from, payload))
net.connect_to("https://<node>", grant_from_backend)
net.join_or_create("lobby-1", AurixNetClient.MODE_RELAY, 8)
net.send_event(1, "hi".to_utf8_buffer()); net.flush()
```

## Signals added for operator messages

- `notice(text: String, level: String)` — system message pushed by the operator console or the game backend (`level` is `info` or `warn`). Show it in the HUD; it is not a chat message.
- `rehost()` — the room moved to another node during a rolling deploy. Call `rejoin()` with the same grant (the node list from `/v1/regions` works as fallback).

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
```
`send_input(client, bytes)` is the raw input for rooms with server-side `logic`.
