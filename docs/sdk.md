# SDKs

| SDK | Path | Transport | Status |
|---|---|---|---|
| Rust | `crates/net-client` | QUIC + WS tunnel | verified (`examples/relay_echo`) |
| C ABI | `crates/net-ffi` (`libaurix_net`, `include/aurix_net.h`) | via Rust client | verified (`ctest/echo.c`) |
| C ABI, aurix-sim | `libaurix_net` also exports `sim_*`/`sim3_*` (`include/aurix_sim.h`, copies in `sdk/cpp`, `sdk/godot/src`) | native | verified (`ctest/sim.c`: six golden hashes) |
| C++ (header-only) | `sdk/cpp/aurix_net.hpp` | via C ABI: relay, sync (`aurix::net::Sync`), lockstep | verified (`echo_test.cpp`, `sync_test.cpp`) |
| Unity (UPM) | `sdk/unity` | pure C# (WebSocket control + tunnel), optional P/Invoke QUIC | managed core live-tested with dotnet; `NetRunner`/`NetView`/`NetTransform`/`[Sync]`/`[Rpc]`, `LockstepClient` + editor window written, real editor build pending; `NetRigidbody`/`NetAnimator`/`TickTimer`/`ISpawned`/`IDespawned` compile-checked (BuildCheck), editor run pending a license |
| Godot 4 GDExtension | `sdk/godot` | C++ wrapper over C ABI | verified (godot-cpp 4.2 build, headless Godot 4.2.2: `sdk/godot/check~`) |
| Web (`@aurix/net`) | `sdk/web` | WS control + WS tunnel (WebTransport next) | verified (`npm test`) |

All SDKs share one shape: `connect(http, grant)` → `joinOrCreate/join` → per frame `poll()`,
`sendEvent/sendData`, `flush()`; events `control` (JSON), `event` (relay frame), `data` (raw
sync/lockstep frames), `closed`. Grants always come from the developer's backend.

## Rehost

When a node shuts down (deploy) it hands its rooms to another node and sends `left{reason:"rehost"}`. Clients rejoin the same room with the same grant: Rust `client.rejoin(&[fallback_urls])`, Web `client.rejoin(fallbackUrls)` (or listen to the `rehost` event), C# `ManagedClient.RejoinAsync(fallbacks)`; Unity `NetRunner` does it automatically (`FallbackUrls`). Actor ids are preserved by player id.

## Lobby statistics and presence

lobby statistics and find-friends, one control round-trip each (at most one per second per
session, both fan out across the fleet):

- Web: `await client.lobbyStats()` → `{rooms, players, lobbies: [{lobby, rooms, players}]}`;
  `await client.findFriends(["ann", "bob"])` → `[{player, online, room?: {id, name, mode, node}}]`.
- Unity (managed): `await client.StatsAsync()` / `await client.FindFriendsAsync(new[]{"ann","bob"})` return
  the JSON answers (`Json.Str/Num/Obj` helpers read them).
- Rust: `client.stats().await?` / `client.find_friends(vec![...]).await?`; the answers arrive as
  `Event::Control(ServerMsg::Stats { .. })` / `ServerMsg::Friends { .. }`.

A room's lobby is its room prop `lobby` (always visible in listings); rooms without one form the default
lobby `""`. See docs/modes.md.

## Lockstep rooms from Rust, C++ and Godot

`net_client::lockstep::LockstepClient` is the same core the C# and Web SDKs use, as pure logic: feed
every data-path payload to `receive()` and act on the events it returns (`Tick` with the confirmed
inputs in tick order, `Restore` on catch-up, `CheckpointRequested` for the master, `DumpRequested`
after a desync); send what `send_input()`, `hash_frame()`, `checkpoint_frame()` and `dump_frame()`
hand back (`Outgoing::reliable` picks the channel). Input delay adapts to the node's margin reports
(`0x25`) between `input_delay_min/max`; `input_redundancy` (1–8) past inputs ride in every datagram.

The C ABI mirrors it (`aurix_net_lockstep_*` in `aurix_net.h`) and the header-only
`aurix::net::Lockstep` wraps it for C++ and the Godot extension:

```cpp
aurix::net::Client c; c.connect(http, grant);
c.joinOrCreate("duel", aurix::net::Mode::Lockstep, 2);
aurix::net::Lockstep ls;
// per frame
ls.sendInput(c, input, sizeof input);
c.poll(); // for each Data event: if (!ls.receive(ev.data, ev.len)) handleRelay(ev);
for (auto e = ls.poll(); e.kind != aurix::net::Lockstep::Kind::None; e = ls.poll()) {
  switch (e.kind) {
    case aurix::net::Lockstep::Kind::Tick:    sim.step(e.tick, e.inputs); aurix::net::Lockstep::reportHash(c, e.tick, sim.hash()); break;
    case aurix::net::Lockstep::Kind::Restore: sim.restore(e.state, e.stateLen); break;
    case aurix::net::Lockstep::Kind::CheckpointReq: aurix::net::Lockstep::sendCheckpoint(c, e.tick, sim.hash(), sim.bytes(), sim.size()); break;
    case aurix::net::Lockstep::Kind::DumpReq: aurix::net::Lockstep::sendDump(c, e.tick, sim.hashAt(e.tick), sim.bytesAt(e.tick), sim.sizeAt(e.tick)); break;
    default: break;
  }
}
```

## Sync rooms from C++ and Godot

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
// world.sendInput(c, bytes, n) for rooms with server-side `logic`
```

Godot: `AurixNetSync` (RefCounted) — `setup(schema)`, feed the client's `data` signal to
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
(C#). Your simulation implements `step(tick, inputs)`, `snapshot()`, `restore(snapshot)`; keep
snapshots cheap (value copies of the game state) because one is taken per predicted tick.

```ts
const ls = new LockstepClient((ch, p) => c.sendData(ch, p));
const rb = new LockstepRollback(game, joined.actor, 0, 8);   // up to 8 predicted ticks
ls.onTick = (t, inputs) => { rb.confirm(t, inputs); ls.reportHash(t, hashOf(rb.confirmedSnapshot())); };
ls.onRestore = (t, _h, state) => { game.load(state); rb.reset(t); };
// per frame
const tick = ls.sendInput(input); rb.localInput(tick, input);
rb.predict(ls.serverTick + ls.inputDelay);                    // run up to where our input will land
render(game);                                                 // predicted state, no input lag
```

Rollback counters (`rollbacks`, `resimulatedTicks`, `predictedTicks`) tell you how often remote
players change their input unexpectedly; a high ratio means lower `maxPrediction` or smoothing in
the view. Hash reports must be computed on the *confirmed* state (`confirmedSnapshot()` after `confirm`),
never on the predicted head — the head is speculative and differs between players.

## Instant replay (kill cam)

Two ways to rewind a lockstep match:

- **Locally, no round trip** — `ReplayRing` (Rust `net_client::rollback::ReplayRing`, Web/C# `ReplayRing`)
  keeps the last `keep` confirmed ticks' inputs and a snapshot of the confirmed state every `every`
  ticks. Record from the confirmed path, then `window(from)` gives the nearest snapshot at or before
  `from` and the inputs to step from it:

  ```ts
  const ring = new ReplayRing(300, 10);           // 10 s at 30 Hz, snapshot every 10 ticks
  ls.onTick = (t, inputs) => { const before = rb.confirmedSnapshot(); rb.confirm(t, inputs); ring.record(t, inputs, () => before); };
  // kill cam: last 3 s
  const w = ring.window(rb.confirmedTick - 90);
  if (w) { cam.restore(w.snapshot); let t = w.start; for (const i of w.inputs) cam.step(t++, i); }
  ```

- **From the node** — `GET /v1/rooms/:id/replay?from=&to=` returns the confirmed inputs of a *live*
  room from whichever node hosts it (and windows the stored replay after close). Use it for
  spectators who arrived late, bug reports and server-side highlight generation; the payload is the
  same `Replay` JSON as the post-match replay (`from_tick`, `inputs`, `hashes`).


## Aurix Sim — deterministic physics in every SDK

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
`character(body)` wraps a kinematic capsule in a controller — `moveCharacter(k, delta)` before
`step()` slides along geometry, steps over ledges up to `stepHeight`, snaps to the ground and
reports `characterState(k).grounded`. C#: `JointBall/JointHinge/JointDistance/HingeAngle/
Character/MoveCharacter/CharacterState`. Reference hash `sim3_joint_golden_hash()` =
`0x5db56af63cceee85` on native and wasm32.

Bot kit ports: Web `BehaviorTree/StateMachine/Blackboard` (`sdk/web/src/bots.ts`) and C#
`Aurix.Net.Sim.Bots` load the same JSON as the Rust `net-sim-bots` crate and produce identical
per-tick statuses and hashes — verified against `crates/net-sim-bots/fixtures/vectors.json`
(regenerate with `BOTS_WRITE_VECTORS=1 cargo test -p net-sim-bots vectors`).

3D convex hulls and sweeps: `addConvex(kind, pos, verts, faces)` / C# `AddConvex` build a convex
polyhedron from <= 32 body-space vertices and faces given as index loops (winding is normalised,
author vertices around the centre of mass; mass is exact volume, inertia the bounding box) — hulls
collide with spheres, capsules, boxes and each other via SAT over face normals and edge crosses with
reference-face clipping. `sphereCast(from, dir, radius)` / `SphereCast` sweeps a sphere and reports
the first body, the centre's stop fraction and the surface point/normal. Reference hash
`sim3_hull_golden_hash()` = `0x29ab0385d38dc53f` on native and wasm32.

### aurix-sim from C / C++ / Godot

`libaurix_net` (crates/net-ffi) links `net-sim-wasm`, so the same flat ABI the Web and Unity
wrappers use is callable natively: `include/aurix_sim.h` (generated by `tools/gen_sim_header.py`,
96 functions: `sim_*` 2D physics/joints/casts, `sim_nav_*`/`sim_agent_*`/`sim_crowd_*` navigation,
`sim3_*` 3D physics/joints/character/convex hulls/sphere casts, and the six `*_golden_hash()`
scenes). Values are raw Q32.32 (`aurix_fx_from_double`/`aurix_fx_to_double`); handles and ids are
the same numbers the Web/Unity wrappers pass through. A GDExtension can call it directly from
`aurix_net_client.cpp`'s neighbourhood (header copied to `sdk/godot/src`); `crates/net-ffi/ctest/sim.c`
is the offline check (golden hashes + a resting ball in 2D and 3D + a sphere cast).
