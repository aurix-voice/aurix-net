# Local verification

Everything below runs on one machine against `deploy/docker-compose.dev.yml` (Postgres +
Redis). Build once: `cargo build --release`. Create a project and key with the operator API
(`aurix-net project-create`, needs `NET_OPERATOR_TOKEN` = `NET__AUTH__ADMIN_TOKEN`) and export
`NET_API_KEY`.

| What | Command | Expect |
|---|---|---|
| unit + integration | `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` | green |
| relay load | `aurix-net-loadtest --mode relay --rooms 100 --players 10 --rate 20` | loss < 1%, p99 < 10 ms (release) |
| relay via WebSocket tunnel / WebTransport | add `--tunnel` / `--webtransport` | same, slightly higher latency |
| sync (built-in arena) | `--mode sync --rooms 4 --players 64 --tick-hz 60` | server tick p99 <= 8 ms, ~60 deltas/s/player |
| sync with WASM logic | `aurix-net module-put arena crates/net-game/examples/arena.wat`, then `--mode sync --props-json '{"schema": "...", "logic": "arena"}'` | same delta rate as built-in |
| lockstep + late join | `--mode lockstep --rooms 4 --players 4 --late-join-secs 12` | `caught-up = rooms`, no `catch-up gap` |
| node failover | `scripts/two-nodes.sh start`, `scripts/failover-test.sh` | room adopted by B, joiners caught up |
| cross-node matchmaking | file one ticket on :8090 and one on :8091 (same shape, `min_players: 2`) | both `matched` to the same room, readable from either node |
| browser WebTransport | serve `sdk/web` (`python3 -m http.server 8765`), open `test/browser.html?http=...&ga=<grant>&gb=<grant>&room=x` in Chrome | `path=webtransport/webtransport hello=1 ticks=10/10` |
| Web SDK (Node, tunnel) | `cd sdk/web && NET_API_KEY=... npm test` | pass |
| C / C++ ABI | `crates/net-ffi/ctest/echo.c`, `sdk/cpp/echo_test.cpp` against a live node | `hello=1 ticks=10/10` |

Known limits of the local setup: bots and server share CPU, so the 500 events/s/room figure
needs bots on a second machine; the dev certificate changes on every node start (browser pins
the fingerprint from `joined.data`, nothing to install).
