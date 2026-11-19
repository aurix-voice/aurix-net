# SDKs

| SDK | Path | Transport | Status |
|---|---|---|---|
| Rust | `crates/net-client` | QUIC + WS tunnel | verified (`examples/relay_echo`) |
| C ABI | `crates/net-ffi` (`libaurix_net`, `include/aurix_net.h`) | via Rust client | verified (`ctest/echo.c`) |
| C ABI, aurix-sim | `libaurix_net` also exports `sim_*`/`sim3_*` (`include/aurix_sim.h`, copies in `sdk/cpp`, `sdk/godot/src`) | native | verified (`ctest/sim.c`: six golden hashes) |
| C++ (header-only) | `sdk/cpp/aurix_net.hpp` | via C ABI: relay, sync (`aurix::net::Sync`), lockstep | verified (`echo_test.cpp`, `sync_test.cpp`) |
| Unity (UPM) | `sdk/unity` | pure C# (WebSocket control + tunnel), optional P/Invoke QUIC; every platform; a Web build goes through the browser's WebSocket and timers (a `.jslib` plug-in) and `UnityWebRequest` | managed core live-tested with dotnet; the Web side checked on a page played by the test, its plug-in linked with Emscripten under Node, and Web builds of Unity 2022.3 and 6000.3 run in Chrome against a node (connect, join, reliable event, round trip, close); `NetRunner`/`NetView`/`NetTransform`/`[Sync]`/`[Rpc]`, `LockstepClient` + editor window compile in the editors of Unity 2022.3 and 6000.3, where the RPCs of a probe are woven and a player builds for Linux and for Web, not yet run in a game in Play mode; `NetRigidbody`/`NetAnimator`/`TickTimer`/`ISpawned`/`IDespawned` compile-checked (BuildCheck) |
| Unreal Engine (beta) | `sdk/unreal` | C++ plugin over C ABI: NetDriver over relay rooms + Blueprint subsystem | beta: written for UE 5.3-5.8, engine-free checks in CI (`sdk/unreal/scripts/check_plugin.py`); built and run with UE 5.8.2 on Linux only |
| Godot 4 GDExtension | `sdk/godot` | C++ wrapper over C ABI | verified (godot-cpp 4.2 build; headless Godot 4.2.2 and 4.4.1: relay, sync and lockstep in `sdk/godot/check~`) |
| Web (`@aurix/net`) | `sdk/web` | WS control + WebTransport or WS tunnel | verified over the tunnel (`npm test`) |

All SDKs share one shape: `connect(http, grant)` → `joinOrCreate/join` → per frame `poll()`,
`sendEvent/sendData`, `flush()`; events `control` (JSON), `event` (relay frame), `data` (raw
sync/lockstep frames), `closed`. Grants always come from the developer's backend.

Outside a room and after `closed` a send fails with `disconnected` and nothing is queued: an
exception in Web, C# and C++, a non-zero result in C, `false` in Godot.

The data path is lost (`closed` with `link dead`) when a reliable message has been resent and
has gone 15 s without the node's acknowledgement, and when more than 1 MiB of them wait for the
node: the send that meets the limit fails ([modes.md](modes.md)). A message whose 15 s ran out
before it was resent once, because the client's timer came late (a page that was busy or frozen,
a hidden tab whose timers the browser slowed), is resent and waits 15 s more. The node applies
the same rule to its own messages: when it gives the link up, the game gets the same `closed`
with `link dead`, over QUIC, WebTransport and the tunnel alike.

A client that sends nothing stays connected: every client probes the control channel once a
second, and the node closes one it has heard nothing from for 90 s ([api.md](api.md)).

A game that stops polling for a while keeps its link. The Rust client, the C, C++, Godot, Unreal
and native C# clients built on it, and the managed C# client read, acknowledge and resend off
the game thread; the poll call only hands the game what has arrived, in order. They hold 4096
events for the game (`ManagedClient.EventBacklog` in C#): with that many waiting they stop
reading until the game polls, what the node sends meanwhile is not acknowledged, and by the
rule above the link is lost after 15 s.

A connected client knows the player its grant names, as the node answered the sign-in: the id
the node uses for the player in rooms, held seats and webhooks, and the display name.

| SDK | Id and name |
|---|---|
| Web | `client.playerId`, `client.playerName` |
| Unity | `client.PlayerId`, `client.PlayerName`, on the managed and the native client |
| C | `aurix_net_player_id(client)`, `aurix_net_player_name(client)`, freed with `aurix_net_string_free` |
| C++ | `client.playerId()`, `client.playerName()` |
| Godot | `net.get_player_id()`, `net.get_player_name()` |
| Unreal | `GetPlayerId`, `GetPlayerName`; the id is the unique net id of the session interface |
| Rust | `client.player_id()`, `client.player_name()` |

`sendEvent` chooses between reliable and unreliable; `sendEventOn` takes the channel, which is how an
event goes on the unsequenced one (docs/modes.md):

| SDK | Call |
|---|---|
| Web | `client.sendEventOn(Channel.UnreliableUnsequenced, code, payload)` |
| Unity | `client.SendEventOn(Channel.UnreliableUnsequenced, code, payload)`, also on `NetRunner` |
| C | `aurix_net_send_event_on(client, AURIX_NET_CH_UNRELIABLE_UNSEQUENCED, code, ...)` |
| C++ | `client.sendEventOn(Channel::UnreliableUnsequenced, code, data, len)` |
| Godot | `net.send_event_on(AurixNetClient.CHANNEL_UNRELIABLE_UNSEQUENCED, code, payload)` |
| Unreal | `SendEventEx` with `EAurixNetChannel::UnreliableUnsequenced` |
| Rust | `client.send_event_on(Channel::UnreliableUnsequenced, code, ...)` |

With a node that does not list `unsequenced` in its `hello` the SDK sends the same event on the
sequenced channel. Sync and lockstep input always goes on the sequenced channel.

A relay event sent with `cache: add` stays in the room for late joiners. They receive it as an
ordinary `event` right after the join, before anything sent later (Web and the managed C# client
mark it `cached`).

In the C ABI a call that fails (`NULL`, `-1`) leaves the reason in two places for the calling
thread: `aurix_net_last_error()` has the text and `aurix_net_last_error_code()` one of the
`AURIX_NET_ERR_*` constants of `aurix_net.h`, so a refused join is told apart without reading the
text (`AURIX_NET_ERR_NOT_FOUND`, `AURIX_NET_ERR_ROOM_FULL`, `AURIX_NET_ERR_ROOM_CLOSED`, ...). The
C++ wrapper throws `aurix::net::Error` with the same value in `code`; the Godot nodes return it
from `get_last_error_code()` (`AurixNetClient.ERROR_*`). A wrapper that runs a call on a worker
thread and reports its failure on another carries the two over with
`aurix_net_set_last_error(code, message)`, as the Unreal plugin does for its async calls.

## Rehost

When a node shuts down (deploy) it hands its rooms to another node and sends `left{reason:"rehost"}`. Clients rejoin the same room with the same grant: Rust `client.rejoin(&[fallback_urls])`, Web `client.rejoin(fallbackUrls)` (or listen to the `rehost` event), C# `ManagedClient.RejoinAsync(fallbacks)`; Unity `NetRunner` does it automatically (`FallbackUrls`). The rejoin is a `join` by the name of the room the client was in, whichever call led it there: a random join or a `join_or_create` is not asked again. C++, Godot and Unreal have no `rejoin` call: connect again with the same grant and join the same room (Unreal reports the `left` with `OnLeft(Reason, RoomId)`). Actor ids are preserved by player id.

## Lobby statistics and presence

lobby statistics and find-friends, one control round-trip each (at most one per second per
session, both fan out across the fleet):

- Web: `await client.lobbyStats()` → `{rooms, players, lobbies: [{lobby, rooms, players}]}`;
  `await client.findFriends(["ann", "bob"])` → `[{player, online, room?: {id, name, mode, node}}]`.
- Unity (managed): `await client.StatsAsync()` / `await client.FindFriendsAsync(new[]{"ann","bob"})` return
  the JSON answers (`Json.Str/Num/Obj` helpers read them).
- Rust: `client.stats(id).await?` / `client.find_friends(id, vec![...]).await?`; the answers arrive as
  `Event::Control(ServerMsg::Stats { id, .. })` / `ServerMsg::Friends { id, .. }`, a refusal as
  `ServerMsg::Error { id, .. }`, each with the `id` of its request.
- One-shot room list and WebRPC: Web `await client.rooms("map = 'dust'")` / `await client.rpc("inventory.get", {slot: 3})`
  → `{ok, status, body, error?}`; Unity `RoomsAsync(expr)` / `RpcAsync(name, bodyJson)`; Rust
  `client.rpc(id, name, body)` with the answer as `ServerMsg::RpcResult { id, .. }`. `rpc` reaches the
  project's webhook subscribed to `rpc` (docs/api.md → Webhooks); the client never sees its URL.
- Lookups and `rpc` calls in flight at once: Web and Unity send each under an id of its own, so every
  call gets its own answer, and the node's refusal of one (`lookup_rate`, `rpc_rate`) fails that call
  only.

- A live room list: send the control message `lobby{subscribe: true}` (Web `client.send(...)`, Unity
  `SendControlAsync` / `SendControl`, C++ `sendControl`, Godot `send_control`) and read the answers
  as control messages (`control` in Web and Godot, `OnControl`, `onControl`). `lobby_rooms{rooms}`
  is the whole list and replaces the one the game holds: it comes once on subscribing and again,
  unasked, whenever the node has reason to send the list anew. `lobby_update{room, removed}` then
  adds, changes or removes one room. The SDKs keep no list of their own. In Unreal
  `SubscribeLobby` does this, `OnRoomsListed` gets each whole list and `OnLobbyUpdate` each change.

- A control message the game writes itself may carry only the fields docs/api.md names for it:
  one with any other field is refused with `invalid_request`, and the refusal names the field.
  Web and the managed C# client hear it from the node (the call fails, or an `error` control
  message comes for a message that has no answer). C, C++, Godot, Unreal and the native C#
  client hear it from the library, which does not send such a message:
  `AURIX_NET_ERR_INVALID_REQUEST`, `Error::code`, `get_last_error_code()`,
  `EAurixNetError::InvalidRequest`, a `NetException`. `props`, `player_props`, `filter`,
  `expected` and `body` are the game's own and take any key.

A room's lobby is its room prop `lobby` (always visible in listings); rooms without one form the default
lobby `""`. See docs/modes.md.

Other room props stay private unless the room publishes them: `joinOrCreate` takes the names as
`lobbyProps` (Web, C#, C++; `lobby_props` in Godot). Room listings show only those props, and the
`expr` filters of `rooms` and `joinRandom` match only those.

`joinRandom(filter, expr, matchType)` joins a random open room: Web and C++ `joinRandom`, Unity
`JoinRandomAsync` / `JoinRandom`, Godot `join_random`. `joinOrCreate` also takes `reserved`, the
player ids whose seats the new room holds for 60 s: the `players` of a matchmaking ticket
(docs/api.md → Matchmaking flow).

`joinOrCreate` takes `visible` and `open` last, both true unless given: Web, C++ and Unity
(`JoinOrCreateAsync`, `JoinOrCreate`, and `JoinOrCreateAsync` of `NetRunner`), Godot
`join_or_create`, Unreal `Join Or Create` (`bVisible`, `bOpen`); in C they are fields of the
`join_or_create` message given to `aurix_net_join`. A room with `visible: false` is out of room
listings and random joins and is entered by its name. A room with `open: false` lets in the
player whose call created it and nobody after: `join` and `joinOrCreate` are refused with
`room_closed`. Like the mode, the size and the props, the two shape the room only when the call
creates it: a room that already exists stays as it is, whatever the call says.

## Lockstep rooms from Rust, C++ and Godot

`net_client::lockstep::LockstepClient` is the same core the C# and Web SDKs use, as pure logic: feed
every data-path payload to `receive()` and act on the events it returns (`Tick` with the confirmed
inputs in tick order, `Restore` on a catch-up and after a desync, `CheckpointRequested` for the
master, `DumpRequested` after a desync); send what `send_input()`, `hash_frame()`,
`checkpoint_frame()` and `dump_frame()` hand back (`Outgoing::reliable` picks the channel). After
every bundle frame send `ack_frame()`
(`0x29`, the tick needed next): the room then resends what a run of lost packets took, instead of
only the last three ticks. The C API, the C++ wrapper, Godot, C# and Web send it themselves, the
first three through the client passed to `receive`. Input delay adapts to the node's margin reports
(`0x25`) between `input_delay_min/max` (1 and 32 ticks): it settles at the round trip in ticks plus
one or two ticks of slack. `input_redundancy` (1–8) past inputs ride in every datagram. `send_input()`
sends nothing until the first frame from the room has told the client the room's tick.

The C ABI mirrors it (`aurix_net_lockstep_*` in `aurix_net.h`) and the header-only
`aurix::net::Lockstep` wraps it for C++ and the Godot extension:

```cpp
aurix::net::Client c; c.connect(http, grant);
c.joinOrCreate("duel", aurix::net::Mode::Lockstep, 2);
aurix::net::Lockstep ls;
// per frame
ls.sendInput(c, input, sizeof input);
c.poll(); // for each Data event: if (!ls.receive(c, ev.data, ev.len)) handleRelay(ev);
for (auto e = ls.poll(); e.kind != aurix::net::Lockstep::Kind::None; e = ls.poll()) {
  switch (e.kind) {
    case aurix::net::Lockstep::Kind::Tick:    sim.step(e.tick, e.inputs); aurix::net::Lockstep::reportHash(c, e.tick, sim.hash()); break;
    case aurix::net::Lockstep::Kind::Restore: sim.restore(e.state, e.stateLen); break;
    case aurix::net::Lockstep::Kind::CheckpointReq: aurix::net::Lockstep::sendCheckpoint(c, sim.tick(), sim.hash(), sim.bytes(), sim.size()); break;
    case aurix::net::Lockstep::Kind::DumpReq: aurix::net::Lockstep::sendDump(c, e.tick, sim.hashAt(e.tick), sim.bytesAt(e.tick), sim.sizeAt(e.tick)); break;
    default: break;
  }
}
```

A checkpoint is labelled with the tick the simulation runs next (`sim.tick()` above: the last `Tick`
it stepped, plus one), not with the tick in the request: the room can be ahead of the client, and a
late joiner restored from a mislabelled state desyncs. `Restore` with tick 0 and an empty state means
"start from the initial state". `sendInput` returns the stamped tick, or -1 when this room tick
already has your input (the room takes one per tick).

The initial state of a game with random numbers comes from the room's seed, the same for every
client and for the node when it runs the game itself. It comes with the join: `Joined::seed` in
Rust, `aurix_net_joined_seed()` in C, `Client::seed()` in C++, `get_seed()` in Godot,
`ManagedClient.Seed` in C# and `joined.seed` on the Web.

## Sync rooms from C++ and Godot

Inputs are stamped ahead of the last delta by a lead: two ticks plus the measured round trip, then
corrected by the room's input margin reports (`0x25`, every 30 ticks): a late report adds the
lateness plus one tick, three reports with ticks to spare give the excess back. All SDKs do this
inside their sync helper (`aurix_net_sync_receive` returns 2 for a report, `Sync::receive` returns
true), so a game never sees the frame.

Remote entities are drawn at the render tick: `interpolationTicks` (2 by default) behind the
last delta, moved on with frame time by `advance(dt)` once a frame. It never passes the tick of
the last delta: when deltas come late or the node stands still it stops there, on the newest
sample, and the next delta puts it back `interpolationTicks` behind, so there are again two
samples to interpolate between. A delta that finds it short of the last one's tick only raises it
when it has fallen more than `interpolationTicks` behind. Web `SyncSession.advance` /
`renderTick`, C++ `Sync::advance` / `renderTick`, Godot `AurixNetSync.advance` /
`get_render_tick`, Unity `NetRunner.RenderTick` (`NetSettings.InterpolationTicks`).

`aurix_net_sync_*` (in `aurix_net.h`) is the client side of a sync room as pure logic: a replicated
world (`net_sim::Frame` + the same delta decoder the node uses), acks, redundant input frames and
owner-authoritative component writes. `aurix::net::Sync` wraps it:

```cpp
aurix::net::Client c; c.connect(http, grant);
c.joinOrCreate("arena", aurix::net::Mode::Sync, 8, "{\"schema\":" + aurix::net::Client::quote(SCHEMA) + "}");
aurix::net::Sync world(SCHEMA, 60, 2);
int tr = world.component("Transform"), pos = world.field(tr, "pos");
c.onData = [&](auto, auto, const uint8_t* d, size_t n) { world.receive(c, d, n); }; // applies + acks
// per frame:
c.poll(); world.advance(dt);
for (auto e = world.poll(); e.kind != aurix::net::Sync::Kind::None; e = world.poll()) {
    float p[4]; if (world.vector(e.entity, tr, pos, p) == 3) { /* spawn/change/despawn of e.entity */ }
}
float mine[3] = {x, y, z};
world.begin(tr).write(pos, mine, 3).end().sendWrites(c);   // owner-authoritative rooms
world.begin(tr).write(pos, mine, 3).end().spawn(c);        // a second entity you own; despawn/transfer/writeEntity likewise
// world.sendInput(c, bytes, n) for rooms with server-side `logic`
// Web: session.spawn(writes) / writeEntity(id, writes) / despawn(id) / transfer(id, actor); Unity: NetRunner.SpawnEntity(writes) / SendOps(SyncOp[...])
```

A spawn returns a tag, not the entity: the room picks the entity and answers with it. The SDK
counts the tags and repeats the spawn until the room answers, and the room makes one entity per
tag, so a lost spawn is not made twice. The answer says `created` with the entity and the tick
the spawn ran at, `refused` (the actor has 32 entities, or the room is full) or `invalid` (the
room could not read the spawn). A refused spawn is not repeated: spawn again when there is room.
Tag 0 is the answer that names the avatar. The answer and the delta that carries the entity
travel on different channels: either may come first, so an answered entity may not be in the
world yet. A room that does not answer tagged spawns (no schema, a `logic` module, an older node)
and a spectator fail the spawn at the call.

| | spawn | answer | avatar |
|---|---|---|---|
| C | `aurix_net_sync_spawn` | `aurix_net_sync_poll_spawn` | `aurix_net_avatar` |
| C++ | `Sync::spawn` | `Sync::pollSpawn` | `Client::avatar` |
| Godot | `AurixNetSync.spawn_entity` | signal `spawn_answer(tag, status, entity, tick)` | `AurixNetClient.get_avatar` |
| Web | `SyncSession.spawn` | `onSpawnAnswer` | `Client.avatar` |
| Unity | `NetRunner.SpawnEntity` | `OnSpawnAnswer` | `NetRunner.Avatar`, `ManagedClient.Avatar` |

The room announces the avatar once, right after the join. The client keeps it from then on, so
the avatar answers whenever the game asks, also when the game set up its sync world later.

Godot: `AurixNetSync` (RefCounted): `setup(schema)`, feed the client's `data` signal to
`receive(client, payload)`, listen to `spawn(entity, owner)` / `change` / `despawn`, read
`get_field(entity, "Transform", "pos")` as Vector3/Quaternion/float/String, write with
`begin_write("Transform")`, `write_field("pos", Vector3(...))`, `end_write()`, `send_writes(client)`.
`sdk/cpp/sync_test.cpp` is the live check (two clients, writes replicate, no gaps).

## Predict/rollback on lockstep (zero-lag local input)

Plain lockstep renders a tick only once the node has confirmed everybody's input, so local actions
show up `input_delay` ticks late. The rollback runner removes that: the game runs ahead of the
confirmed tick with *predicted* inputs (every remote actor's last confirmed input repeated, your own
inputs exactly as sent), keeps a snapshot per predicted tick, and when a confirmed tick differs from
the prediction it restores the snapshot, re-steps that tick and re-predicts the ticks after it. The
confirmed state only ever sees confirmed inputs, so determinism and hash reports are untouched.

Same core in three SDKs: `net_client::rollback::Rollback<S: Simulation>` (Rust),
`LockstepRollback<S>` + `Simulation<S>` (Web), `LockstepRollback<TSnapshot>` + `ISimulation<TSnapshot>`
(C#). Your simulation implements `step(tick, inputs, commands)`, `snapshot()`, `restore(snapshot)`;
keep snapshots cheap (value copies of the game state) because one is taken per predicted tick.

`commands` are the [lockstep commands](#lockstep-commands) of the tick, each with its sender, in
the order every client gets them: run them first, then the inputs. Commands are not predicted. A
predicted tick has none, and a confirmed tick that carries some is stepped again with them even
when the inputs were guessed right. A game that predicts takes its commands here and nowhere
else: one applied from `onCommand` is lost at the next rollback and is not in the replay. This
third argument is a breaking change for a simulation written as `step(tick, inputs)`.

| | step | confirm |
|---|---|---|
| Rust | `Simulation::step(tick, &inputs, &[Command])` | `confirm(&mut sim, tick, &inputs)` with the inputs of `LsEvent::Tick`; it takes the commands out itself (`lockstep::take_commands`) |
| Web | `step(tick, inputs, commands)` | `onTick(tick, inputs, commands)`, `confirm(tick, inputs, commands)` |
| C# | `Step(tick, inputs, commands)` | inside `OnTick`: `Confirm(tick, inputs, lockstep.Commands)` |
| Unreal | `OnSimulate(Tick, Inputs, Commands, bPredicted)` of `AurixRollbackRunner` | the runner does it |

```ts
const ls = new LockstepClient((ch, p) => c.sendData(ch, p));
const rb = new LockstepRollback(game, joined.actor, 0, 8);   // up to 8 predicted ticks
ls.onTick = (t, inputs, commands) => { rb.confirm(t, inputs, commands); ls.reportHash(t, hashOf(rb.confirmedSnapshot())); };
ls.onRestore = (t, _h, state) => { game.load(state); rb.reset(t); };
// per frame
const tick = ls.sendInput(input); rb.localInput(tick, input);
rb.predict(ls.serverTick + ls.inputDelay);                    // run up to where our input will land
render(game);                                                 // predicted state, no input lag
```

In C# `LockstepClient.SendInput` returns the same as `sendInput`: the tick the input is stamped
with, or -1 when it sent nothing. Give `LocalInput` the input only with a tick that is not -1.

Rollback counters (`rollbacks`, `resimulatedTicks`, `predictedTicks`) tell you how often remote
players change their input unexpectedly; a high ratio means lower `maxPrediction` or smoothing in
the view. Hash reports must be computed on the *confirmed* state (`confirmedSnapshot()` after `confirm`),
never on the predicted head: the head is speculative and differs between players.

## Instant replay (kill cam)

Two ways to rewind a lockstep match:

- **Locally, no round trip**: `ReplayRing` (Rust `net_client::rollback::ReplayRing`, Web/C# `ReplayRing`)
  keeps the last `keep` confirmed ticks' inputs and commands and a snapshot of the confirmed state
  every `every` ticks. Record from the confirmed path, then `window(from)` gives the nearest
  snapshot at or before `from` and the inputs and commands to step from it:

  ```ts
  const ring = new ReplayRing<State>(300, 10);    // 10 s at 30 Hz, snapshot every 10 ticks; State: the type of rb's snapshots
  ls.onTick = (t, inputs, commands) => { const before = rb.confirmedSnapshot(); rb.confirm(t, inputs, commands); ring.record(t, inputs, commands, () => before); };
  // kill cam: last 3 s
  const w = ring.window(rb.confirmedTick - 90);
  if (w) { cam.restore(w.snapshot); w.inputs.forEach((inputs, i) => cam.step(w.start + i, inputs, w.commands[i])); }
  ```

- **From the node**: `GET /v1/rooms/:id/replay?from=&to=` returns the confirmed inputs of a *live*
  room from whichever node hosts it (and windows the stored replay after close). Use it for
  spectators who arrived late, bug reports and server-side highlight generation; the payload is the
  same `Replay` JSON as the post-match replay (`from_tick`, `inputs`, `hashes`).


## Aurix Sim: deterministic physics in every SDK

`net-sim-physics` (fixed-point Q32.32, no floats) is exposed through one flat ABI crate,
`net-sim-wasm`, built both as `net_sim_wasm.wasm` (browsers, Unity WebGL, node referee) and as a
native `libnet_sim_wasm.{so,dll,dylib}` (Unity desktop/mobile, C++/Godot). Every build runs the
same integer math, so a lockstep room can hash the world on any platform and compare:
`sim_golden_hash()` returns `0x58d9870ba45952b5` on x86-64 native and wasm32.

```bash
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown -p net-sim-wasm   # target/wasm32-unknown-unknown/release/net_sim_wasm.wasm
cargo build --release -p net-sim-wasm                                   # target/release/libnet_sim_wasm.so
cargo run -p net-sim-wasm --example golden_check                        # native vs wasm hash
```

Web:

```ts
import { Sim, BodyKind } from "@aurix/net";
const sim = await Sim.load(fetch("/net_sim_wasm.wasm"));
const w = sim.world({ x: 0, y: -10 }, 60);
w.addBox(BodyKind.Static, 0, -1, 50, 1);
const hero = w.addCapsule(BodyKind.Kinematic, 0, 1, 0.5, 0.5);
const kcc = w.character(hero);            // step-up 0.5, slope limit cos 0.7
const ball = w.addCircle(BodyKind.Dynamic, 3, 5, 0.5);
w.setMaterial(ball, 0.3, 0.6);
// per lockstep tick:
w.moveCharacter(kcc, input.x / 60, vy / 60);
w.step();
lockstep.reportHash(tick, w.hash());      // identical on every client
const snap = w.snapshot();                // for rollback / checkpoints
```

Unity (C#): `Aurix.Net.Sim.SimWorld` has the same surface (`AddBox/AddCircle/AddCapsule`,
`SetMaterial/SetLayers/SetVelocity/ApplyImpulse`, `Body(id)`, `Raycast`, `Character/MoveCharacter`,
`Snapshot/Restore/Hash`). Put `libnet_sim_wasm.so`/`.dll` under `Plugins/` (WebGL links the static
build). Rust games use `net-sim-physics` directly (`World`, `Body`, `Joint`, `CharacterController`),
with joints (distance/rope, revolute with limits and motor, weld) and contact events on top of what
the flat ABI exposes today.

Navigation rides on the same module (`net-sim-nav`): `navGrid`/`navLoad` (JSON from
`NavMesh::to_bytes`), `navPath` (A* over portals + funnel), `navRaycast` (line of sight),
`navField` (one flow field per goal, shared by any number of agents), `addAgent`/`agentTarget`/
`agentFollow`, `crowdStep` next to `step()`, `crowdHash`/`crowdSnapshot` for lockstep checks.
C#: `NavGrid/NavLoad/NavPath/NavRaycast/NavField/AddAgent/AgentTarget/AgentFollow/CrowdStep`.
Reference hash `sim_nav_golden_hash()` = `0x746c5a608abe2f26` on native and wasm32.

```ts
w.navGrid({ x: 0, y: 0 }, 1, 64, 64, blockedCells);
const toBase = w.navField({ x: 60.5, y: 60.5 });
const unit = w.addAgent(2, 2, 0.4, 3);
w.agentFollow(unit, toBase);
// per tick: w.crowdStep(); w.step(); lockstep.reportHash(tick, w.hash() ^ w.crowdHash());
```

Joints, contact events and shape casts are on the same handle: `jointDistance` (rope when
`minLength < length`), `jointRevolute` (limits + motor, `setMotor` to retune), `jointWeld`,
`removeJoint`; `events()` returns the begin/end contact pairs of the last `step()`; `circleCast`
sweeps a circle and reports where it first touches (C#: `JointDistance/JointRevolute/JointWeld/
RemoveJoint/SetMotor/Events/CircleCast`). Reference hash `sim_joint_golden_hash()` =
`0x4bcc781a484ceeed` on native and wasm32.

### 3D worlds

The same module hosts `net-sim-physics3d`: `sim.world3(gravity, hz)` → `Sim3World` with
`addSphere/addBox/addCapsule`, `setMaterial/setLayers/setVelocity/setPosition/setRotation`
(`Sim3World.axisAngle` builds quaternions), `applyImpulse(id, impulse, at?)`, `body(id)`
(position, rotation, velocity, angular velocity), `raycast`, `events`, `snapshot/restore/hash`.
C#: `Aurix.Net.Sim.Sim3World` with the same surface (`Vec3`, `Quat.AxisAngle`, `Body3State`,
`Ray3Hit`). Reference hash `sim3_golden_hash()` = `0x0a9c7e6fc4fa43bf` on native and wasm32.

```ts
const w3 = sim.world3();
w3.addBox(BodyKind.Static, { x: 0, y: -0.5, z: 0 }, { x: 50, y: 0.5, z: 50 });
const crate = w3.addBox(BodyKind.Dynamic, { x: 0, y: 3, z: 0 }, { x: 0.5, y: 0.5, z: 0.5 });
w3.setRotation(crate, Sim3World.axisAngle({ x: 0, y: 0, z: 1 }, 0.3));
// per tick: w3.step(); lockstep.reportHash(tick, w3.hash());
```

3D joints and characters: `jointBall`, `jointHinge` (local axes, optional limits + motor,
`hingeAngle`, `setMotor`), `jointDistance` (rope when `minLength < length`), `removeJoint`;
`character(body)` wraps a kinematic capsule in a controller: `moveCharacter(k, delta)` before
`step()` slides along geometry, steps over ledges up to `stepHeight`, snaps to the ground and
reports `characterState(k).grounded`. C#: `JointBall/JointHinge/JointDistance/HingeAngle/
Character/MoveCharacter/CharacterState`. Reference hash `sim3_joint_golden_hash()` =
`0x5db56af63cceee85` on native and wasm32.

Bot kit ports: Web `BehaviorTree/StateMachine/Blackboard` (`sdk/web/src/bots.ts`) and C#
`Aurix.Net.Sim.Bots` load the same JSON as the Rust `net-sim-bots` crate and produce identical
per-tick statuses and hashes, verified against `crates/net-sim-bots/fixtures/vectors.json`
(regenerate with `BOTS_WRITE_VECTORS=1 cargo test -p net-sim-bots vectors`).

3D convex hulls and sweeps: `addConvex(kind, pos, verts, faces)` / C# `AddConvex` build a convex
polyhedron from <= 32 body-space vertices and faces given as index loops (winding is normalised,
author vertices around the centre of mass; mass is exact volume, inertia the bounding box); hulls
collide with spheres, capsules, boxes and each other via SAT over face normals and edge crosses with
reference-face clipping. `sphereCast(from, dir, radius)` / `SphereCast` sweeps a sphere and reports
the first body, the centre's stop fraction and the surface point/normal. Reference hash
`sim3_hull_golden_hash()` = `0xd656fc8f55999cf8` on native and wasm32. `capsuleCast(from, rot, halfLen, radius, dir)` / `boxCast(from, rot, half, dir)` (Unity `CapsuleCast`/`BoxCast`, C ABI `sim3_capsulecast`/`sim3_boxcast`) sweep a rotated capsule (local axis Y) or box the same way and return the same hit shape.

### aurix-sim from C / C++ / Godot

`libaurix_net` (crates/net-ffi) links `net-sim-wasm`, so the same flat ABI the Web and Unity
wrappers use is callable natively: `include/aurix_sim.h` (generated by `tools/gen_sim_header.py`,
96 functions: `sim_*` 2D physics/joints/casts, `sim_nav_*`/`sim_agent_*`/`sim_crowd_*` navigation,
`sim3_*` 3D physics/joints/character/convex hulls/sphere casts, and the six `*_golden_hash()`
scenes). Values are raw Q32.32 (`aurix_fx_from_double`/`aurix_fx_to_double`); handles and ids are
the same numbers the Web/Unity wrappers pass through. A GDExtension can call it directly from
`aurix_net_client.cpp`'s neighbourhood (header copied to `sdk/godot/src`); `crates/net-ffi/ctest/sim.c`
is the offline check (golden hashes + a resting ball in 2D and 3D + a sphere cast).

## Best region

Every SDK can pick the closest live node before connecting: it reads `GET /v1/regions` from the catalog (or any
node), probes each endpoint's `/healthz` a few times and returns the fastest: Rust `best_region(catalog, probes)`,
C `aurix_net_best_region` (JSON, free with `aurix_net_string_free`), C++ `Client::bestRegionEndpoint`, Godot
`AurixNetClient.best_region`, Web `Client.bestRegion`, Unity `RegionPicker.BestRegionAsync`, Unreal
`UAurixNetSubsystem::BestRegionAsync`. Use the returned endpoint as the cell URL; every candidate with its RTT comes
back too, so a region picker UI is one call.

## Chat

No separate chat service: `Chat` (Web), `ChatClient` (Unity) and `aurix::net::Chat` (C++) run on a relay room.
Channels are interest groups (`join` = subscribe, chat groups live at `groupBase + channel`, default 60000), messages
are reliable events to the group with `cache: add` so late joiners receive the history the room still holds when
they join the channel (the room's cache limits and `cleanup_cache_on_leave` apply), whispers are events to one actor and are never cached.
Payload: `u8 kind (0 channel, 1 whisper), u16 channel, UTF-8 text` on a reserved event code (default 240). Chat
stays inside the room, so moderation (kick, bans), rate limits and GDPR export/erase cover it for free.

## Lockstep commands

`SendCommand(bytes)` (Unity `LockstepClient`, Web `LockstepClient`, C `aurix_net_lockstep_send_command`, C++
`Lockstep::sendCommand`, Unreal `UAurixLockstepComponent::SendCommand`) sends a reliable message outside the input
stream; the cell stamps it onto the next tick and everyone (and the referee) receives it at that tick:
`OnCommand(tick, actor, bytes)` right before `OnTick`, `Event::commands` of the tick in C++, the signal `command`
before `tick` in Godot. C hands the tick as the node sent it: the commands are the input of pseudo-actor 65535
(`AURIX_NET_COMMAND_ACTOR`), which is not a player, and so does the Rust `LsEvent::Tick`
(`lockstep::take_commands` takes them out). A game that predicts gets the commands in the step of its simulation
instead ([predict/rollback](#predictrollback-on-lockstep-zero-lag-local-input)). Commands are journaled, so replays and late-join catch-ups include them. A command is 1 to 251
bytes (`MAX_COMMAND`, `LockstepClient.MaxCommand`, `AURIX_NET_MAX_COMMAND`); an empty or a longer one is refused at
the sender. Commands that do not fit one tick ride the next ticks in the order they were sent. When more than
4096 bytes of them wait, the node refuses the command and the sender gets a control message
`{"t":"error","code":"rate_limited"}`: the `control` event in Web, `OnControl` in C#, `onControl` in C++, the
`control` signal in Godot, `OnError` in Unreal. A command of a wrong length that reaches the node is answered the
same way with `invalid_request`.

## Unity: TickTimer and synchronised scene load

`TickTimer.CreateFromSeconds(nm, 3f)` / `Expired(nm)` is evaluated against the network tick, so every peer agrees
(store it in a SyncVar or predicted state). `NetworkManager.LoadScene(name)` on the server loads the scene on every
peer and raises `onSceneLoadDone` only when all players reported ready (or `sceneLoadTimeout` passed); replace
`sceneLoader` for Addressables or custom streaming.

## Sync-room instant replay

Room props `replay_every` (ticks, default hz/2) and `replay_secs` (default 30, 0 off) keep full-world delta frames on
the node; `GET /v1/rooms/{id}/replay?from=&to=` returns them (`mode: "sync"`). Feed the frames, in order, to a fresh
sync world in any SDK to play back a kill cam, with no extra client state: `SyncWorld.receive(frame)` (Web, Unity),
`aurix_net_sync_receive` (C), `Sync::receive(data, len)` (C++), `receive(null, frame)` (Godot `AurixNetSync`), `ReceiveReplayFrame` on a
`UAurixSyncWorld` with `bReplay` set (Unreal).
