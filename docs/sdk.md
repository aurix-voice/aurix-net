# SDKs

| SDK | Path | Transport | Status |
|---|---|---|---|
| Rust | `crates/net-client` | QUIC + WS tunnel | verified (`examples/relay_echo`) |
| C ABI | `crates/net-ffi` (`libaurix_net`, `include/aurix_net.h`) | via Rust client | verified (`ctest/echo.c`) |
| C++ (header-only) | `sdk/cpp/aurix_net.hpp` | via C ABI | verified (`echo_test.cpp`) |
| Unity (UPM) | `sdk/unity` | pure C# (WebSocket control + tunnel), optional P/Invoke QUIC | managed core live-tested with dotnet; `NetRunner`/`NetView`/`NetTransform`/`[Sync]`/`[Rpc]` + editor window written, real editor build pending |
| Godot 4 GDExtension | `sdk/godot` | C++ wrapper over C ABI | written, needs godot-cpp build check |
| Web (`@aurix/net`) | `sdk/web` | WS control + WS tunnel (WebTransport next) | verified (`npm test`) |

All SDKs share one shape: `connect(http, grant)` → `joinOrCreate/join` → per frame `poll()`,
`sendEvent/sendData`, `flush()`; events `control` (JSON), `event` (relay frame), `data` (raw
sync/lockstep frames), `closed`. Grants always come from the developer's backend.
