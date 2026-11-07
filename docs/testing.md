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
| sync loss repair | `--mode sync --rooms 2 --players 8 --drop-in 0.05` | `client gaps noticed` ≈ dropped deltas, server `net_sync_lost_deltas_total` grows by about the same; no resync, state repaired in full per entity |
| sync with WASM logic | `aurix-net module-put arena crates/net-game/examples/arena.wat`, then `--mode sync --props-json '{"schema": "...", "logic": "arena"}'` | same delta rate as built-in |
| lockstep + late join | `--mode lockstep --rooms 4 --players 4 --late-join-secs 12` | `caught-up = rooms`, no `catch-up gap` |
| node failover | `scripts/two-nodes.sh start`, `scripts/failover-test.sh` | room adopted by B, joiners caught up |
| cross-node matchmaking | file one ticket on :8090 and one on :8091 (same shape, `min_players: 2`) | both `matched` to the same room, readable from either node |
| redirect follow | room on A, `cargo run --release -p net-client --example follow -- http://127.0.0.1:8091 $NET_API_KEY <room>` | joins with data host of A |
| quotas | `UPDATE projects SET limits = '{"max_rooms":1}'`, loadtest `--rooms 2` | second room fails `quota_exceeded: max 1 open rooms` |
| metering | loadtest `--duration-secs 75`, then `aurix-net usage` | room_minutes/player_minutes/bytes rows for the hour |
| browser WebTransport | serve `sdk/web` (`python3 -m http.server 8765`), open `test/browser.html?http=...&ga=<grant>&gb=<grant>&room=x` in Chrome | `path=webtransport/webtransport hello=1 ticks=10/10` |
| Web SDK (Node, tunnel) | `cd sdk/web && NET_API_KEY=... npm test` | pass |
| C / C++ ABI | `crates/net-ffi/ctest/echo.c`, `sdk/cpp/echo_test.cpp` against a live node | `hello=1 ticks=10/10` |

Known limits of the local setup: bots and server share CPU, so the 2000 events/s/room figure
needs bots on a second machine; the dev certificate changes on every node start (browser pins
the fingerprint from `joined.data`, nothing to install).

## Unity managed client (pure C#)

```
cd sdk/unity/BuildCheck~/managed-live && NET_API_KEY=an_… dotnet run -c Release
```
Expects `MANAGED OK`: 1 reliable, 10 unreliable and one 5 KB fragmented relay event delivered
between two clients, and >60 sync deltas in 2 s. Needs a node on 127.0.0.1:8090 with the tunnel on.

## Rolling deploy (SIGTERM handoff)

```
KILL_SIGNAL=TERM scripts/failover-test.sh
```
Same pass criteria as the crash variant, but node A hands its rooms off itself on SIGTERM
(fresh checkpoint, rooms `orphaned`), so players reconnecting 2 s later already find the room
on B. Clients receive `left{reason:"rehost"}` and should rejoin the same room name
(`connect_and_join` / `connectAndJoin`), which lands on whichever node adopts it.

Client-side check of the same path (Rust SDK `rejoin`): `cargo build --example rehost -p net-client`, run
`target/debug/examples/rehost http://127.0.0.1:8090 <grant> <room> http://127.0.0.1:8091`, send SIGTERM
to node A; expected output ends with `rejoined ... (same actor: true)` and `REHOST_OK`.

SDK side of the same handoff: `NET_API_KEY=... scripts/sdk-rehost-test.sh` puts a Web client and a
pure-C# client in one relay room on A, sends A SIGTERM and expects both to see `left{reason:"rehost"}`,
`rejoin()` / `RejoinAsync()` onto B with the same actor id, and to receive an event from a client that
joins on B afterwards (`WEB_REHOST_OK`, `CS_REHOST_OK`). The Rust client has the same check as
`cargo run -p net-client --example rehost`.
