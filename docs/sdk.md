# SDKs

| SDK | Path | Transport | Status |
|---|---|---|---|
| Rust | `crates/net-client` | QUIC + WS tunnel | verified (`examples/relay_echo`) |
| C ABI | `crates/net-ffi` (`libaurix_net`, `include/aurix_net.h`) | via Rust client | verified (`ctest/echo.c`) |
| C++ (header-only) | `sdk/cpp/aurix_net.hpp` | via C ABI | verified (`echo_test.cpp`) |
| Unity (UPM) | `sdk/unity` | pure C# (WebSocket control + tunnel), optional P/Invoke QUIC | managed core live-tested with dotnet; `NetRunner`/`NetView`/`NetTransform`/`[Sync]`/`[Rpc]`, `LockstepClient` + editor window written, real editor build pending |
| Godot 4 GDExtension | `sdk/godot` | C++ wrapper over C ABI | written, needs godot-cpp build check |
| Web (`@aurix/net`) | `sdk/web` | WS control + WS tunnel (WebTransport next) | verified (`npm test`) |

All SDKs share one shape: `connect(http, grant)` → `joinOrCreate/join` → per frame `poll()`,
`sendEvent/sendData`, `flush()`; events `control` (JSON), `event` (relay frame), `data` (raw
sync/lockstep frames), `closed`. Grants always come from the developer's backend.

## Rehost

When a node shuts down (deploy) it hands its rooms to another node and sends `left{reason:"rehost"}`. Clients rejoin the same room with the same grant: Rust `client.rejoin(&[fallback_urls])`, Web `client.rejoin(fallbackUrls)` (or listen to the `rehost` event), C# `ManagedClient.RejoinAsync(fallbacks)`; Unity `NetRunner` does it automatically (`FallbackUrls`). Actor ids are preserved by player id.

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

