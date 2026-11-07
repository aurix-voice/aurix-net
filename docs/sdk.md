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

