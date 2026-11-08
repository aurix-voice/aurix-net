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
| C ABI, aurix-sim | `crates/net-ffi/ctest/sim.c` against `libaurix_net.so` (offline) | six golden hashes equal to Rust/wasm32, 2D/3D rest + sphere cast |

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

## Redis outage (chaos)

Stop Redis for ~12 s in the middle of a lockstep or sync match (`docker stop aurix-redis`, later
`docker start`): play must continue untouched (tick loops never await Redis), the node logs a few
`redis write failed/timed out` warnings during the outage and none after Redis is back, journal keys
reappear, and the first lockstep tick after a lost journal batch writes a fresh checkpoint (an adopting
node replays the journal only up to the first gap). Verified: 4/4 players, 5 warnings in 12 s, 0 after.

## Postgres outage (chaos)

Stop Postgres for ~12 s mid-match: play continues (no tick loop touches the pool), joins/REST fail
fast (5 s pool acquire timeout) instead of hanging, heartbeats and metering resume on their own, and
rooms that emptied meanwhile still close after `player_ttl` + `empty_ttl`. Verified: sync 8/8 players,
only sqlx "slow acquire" warnings, node healthy afterwards.

## Capacity reference (one 8-core dev box, bots and nodes sharing CPU, release build)

| Scenario | Result |
|---|---|
| relay 4 rooms x 32 players @ 20 ev/s, one source IP | 128/128 joined, loss 0.34 %, latency p50 1.8 ms / p99 3.5 ms |
| relay 1 room x 64 players @ 20 ev/s | 64/64 joined, loss 0.49 %, p99 3.4 ms |
| sync 4 x 32 @ 60 Hz | 128/128 joined, server tick mean 0.69 ms, p99 <= 2 ms, ~100 deltas/player/s (byte budget), lost deltas 0.08 % repaired by acks |
| lockstep 4 x 8 | 32/32 joined, tick mean 0.23 ms, desyncs 0 |

All limits at defaults (`[limits]`): these runs are the regression bar for "limits never cut legitimate
players"; a 128-player join burst from one IP must stay at failed=0.

## Soak (1 h, release build, two nodes, bots churning every 5 min)

`scripts/soak-test.sh` runs relay (2 rooms × 8 players @ 20 ev/s, WS tunnel) and sync (1 room × 8 @ 20 Hz)
bots against node B in 300 s batches for an hour — rooms are created, filled, emptied and closed
twelve times — while sampling both nodes' live stats every minute (`/v1/operator/nodes/:id/stats`).

| Node | RSS first / last / max | sync tick p99 max | tick overruns |
|---|---|---|---|
| B (hosting 3 rooms, 24 players) | 20 / 22 / 22 MB | 0.20 ms | 0 |
| A (idle peer: heartbeats, lobby, alert leader) | 13 / 17 / 17 MB | — | 0 |

RSS on both nodes plateaus within the first 30 minutes and stays flat through the remaining room/player
churn: no growth per room lifecycle. Rerun after changes to the room, checkpoint or metering paths;
a steadily climbing RSS across batches is the regression signal.


## Soak 2 — relay + sync + lockstep with forced desyncs (1 h, release, two nodes)

`WEB_KEY=<project key> DURATION=3600 ./scripts/soak-test.sh`: every 5-minute batch runs 2 relay rooms × 8
players (WS tunnel), 1 sync room × 8, 1 lockstep room × 4 at 30 Hz with a late joiner (catch-up from
checkpoint + journal), plus the web `lockstep-live` test whose third client lies about its hash, so the
node collects two desync dumps per batch (stored in Redis for 24 h).

| node | samples | RSS start → end (max) MB | tick p99 max | overruns | rooms / players max |
|---|---|---|---|---|---|
| host (725007) | 61 | 20 → 26 (28) | 0.2 ms | 0 | 7 / 31 |
| idle (087cc9) | 61 | 14 → 18 (18) | 0.2 ms | 0 | 1 / 0 |

Host RSS climbs with the first batches (room churn, dump storage) and stays flat afterwards; no
overruns; desync dumps for every batch were listed and downloadable through `/v1/rooms/:id/desyncs`.

- `NET_HTTP=… NET_API_KEY=… node --test sdk/web/test/referee-live.test.mjs` — lockstep referee: a lying client is flagged by the node's own hash, the node state is stored as desync dump actor 0, room detail reports `referee: true` (upload `crates/net-game/examples/arena.wat` as module `arena` with an admin key first).
- `g++ -std=c++17 sdk/cpp/sync_test.cpp -Isdk/cpp -Ltarget/release -laurix_net -lpthread -ldl -lm -o sync_test && NET_HTTP=… NET_API_KEY=… ./sync_test` — C ABI sync world: two C++ clients in an owner-authoritative sync room, 90 writes of A's Transform replicate to B (85 updates, 87 server ticks, 0 gaps, last position within quantization), A sees exactly one own avatar, render tick lags the server.
- `NET_HTTP=<node A> NET_HTTP_B=<node B> NET_API_KEY=… node --test sdk/web/test/presence-live.test.mjs` — `find_friends` across two nodes (friend in a room on B with room/node, friend only connected on B → online without room, stranger → offline, duplicates collapsed, second request within a second → `lookup_rate`), `stats` with the `ranked` lobby counted, and the idle friend going offline after disconnect.

## Soak 3 — sim referee in lockstep rooms (1 h, release, two nodes)

`WEB_KEY=<key of the project that owns module physics-arena> DURATION=3600 LOCKSTEP_PROPS='{"schema":"…","logic":"physics-arena","position":"Transform.0"}' ./scripts/soak-test.sh`:
the soak 2 pools plus, per 5-minute batch, a second lockstep room (4 bots, late joiner) whose room logic is
the `net-sim-guest` module — the node runs the fixed-point physics itself over confirmed inputs
(`referee: true`) — and the web `referee-live` test (a client lying about its hash every 10 ticks).

**Run 1 (glibc malloc, before the fix) found a real problem**: the host node's RSS stepped up on every
batch, 14 → 189 MB after the first and 264 MB after an hour (p99 tick 0.5 ms, 9 rooms / 34 players max),
while the idle node stayed flat. heaptrack on a node fed six lying-client rounds showed 40 MB peak heap and
140 KB leaked: the memory was freed, glibc just kept its per-thread arenas at their high-water mark. The
bursts came from two places — every desync of the liar made the node build and send a full catch-up
(checkpoint + journal tail), and the first instantiation of a module ran Cranelift on all cores. Fixes:
catch-up only up to `UNTRUSTED_AFTER` strikes, the node binary uses mimalloc, modules compile on one
thread. Reproduce the churn with `sdk/web/test/referee-churn.mjs` (`LIE=1`): before, +3…20 MB per round
and never released; after, a fresh node goes 30 → 100 MB on the first referee room (compile) and then
oscillates 68–76 MB across six lying rounds.

**Run 2 (fixed build, nodes restarted):**

| node | samples | RSS start → end (max) MB | tick p99 max | overruns | rooms / players max |
|---|---|---|---|---|---|
| host (d2eb03) | 62 | 31 → 113 (124) | 0.5 ms | 0 | 9 / 37 |
| idle (f2bb50) | 62 | 31 → 44 (46) | — | 0 | 0 / 0 |

Host RSS is no longer a staircase: 31 MB at start (mimalloc baseline is ~15 MB above glibc), 84–88 MB
through the first four batches, one burst to 118 MB in batch five that mimalloc gave back to 100 MB within
four minutes, and 100–113 MB for the rest of the hour (124 MB peak during the last batch's late join). The
node flagged 276 desyncs from the lying clients (`net_lockstep_desyncs_total`), every referee room reported
`referee: true` and stored its own state as dump actor 0, no tick overruns.

Memory probe before the run (fresh release node, `aurix-net-loadtest --mode lockstep --props-json … --rooms 4
--players 4` repeated 15×, 60 referee rooms created and reaped, no desyncs): RSS 14 MB → 54 MB after the
first instantiation → 64 MB after 60 rooms, flat for the last five rounds and unchanged after all rooms were
reaped; 12 plain lockstep rooms afterwards added 0 MB — the referee rooms themselves do not leak.

## Soak 4 — production image on the compose stack (10 min, distroless, read-only rootfs)

Same pools as soak 3 (`BATCH=120 DURATION=600`, module `physics-arena` uploaded to the stack with
`PUT /v1/modules/physics-arena`) against `deploy/docker-compose.prod.yml` as shipped: two nodes from the
distroless image (uid 65532, `read_only`, `tmpfs /tmp`, `cap_drop: [ALL]`, `no-new-privileges`, nofile
65536), Postgres and Redis on the internal network, console behind Caddy. Point of the run: the hardened
container must not change anything — no writes to the rootfs, no capability the node silently needs, the
built-in `--healthcheck` must keep the container healthy under load.

| node | samples | RSS start → end (max) MB | tick p99 max | overruns | rooms / players max |
|---|---|---|---|---|---|
| host (node-b) | 22 | 29 → 118 (173) | 0.5 ms | 0 | 15 / 37 |
| idle (node-a) | 22 | 31 → 35 (35) | — | 0 | 0 / 0 |

Host RSS: 173 MB on the first sample (first instantiation of the module + the first batch of all pools
starting at once), then 82–93 MB for eight minutes and 101 → 118 MB in the last batch's late join — the
same shape as soak 3 run 2 on bare metal, so mimalloc behaves identically in the container. 207 desyncs
flagged (`net_lockstep_desyncs_total`), Docker health `healthy` throughout, 0 restarts, no OOM, no error
log lines. One stand-specific finding: `GET /v1/operator/nodes/:id/stats` is proxied to the other node via
its `server.public_host`, so with the compose placeholders (`node-b.net.example.com`) it returns 500 until
`PUBLIC_HOST` resolves inside the cluster — documented in deployment.md.

## Robustness of attacker-facing decoders (`tests/garbage.rs` in each crate)

Every decoder that takes bytes from a peer is fed 20 000 random/mutated inputs per test (a seeded
xorshift, no extra dependencies, runs in `cargo test`): relay `EventFrame::decode`, `Schema::parse`,
`ComponentValue::decode_full/decode_delta`, `Replicator::apply`, `LockstepRoom::handle` and
`SyncRoom::handle` (all frame kinds, player and spectator), `Filter::parse`, the lockstep client
`receive`, transport `Header::decode` / `Reassembler::accept` / `Link::receive` and ticket `verify`.
The first run found a real one: a delta or checkpoint naming entity index `u32::MAX` made
`Frame::ensure_entity` resize the slot table to 170 GB (abort) — and `Frame::restore` trusted the
slot count of a client-uploaded checkpoint/dump the same way. Both now reject anything beyond
`FrameConfig::max_entities` (4096). `cargo audit` is clean (477 crates).

## CI (`.github/workflows/ci.yml`)

| job | what runs |
|---|---|
| `rust` | fmt, clippy `-D warnings`, workspace tests, migrations on a fresh Postgres, `net-sim-wasm` wasm32 + native builds and `golden_check` (all six golden hashes native == wasm32), `libaurix_net` + `ctest/sim.c` (C ABI golden check) and the header generator diff; uploads the `.wasm`, `libnet_sim_wasm.so` and the debug `aurix-net-server` for the jobs below |
| `supply-chain` | `cargo audit`, `cargo deny check` |
| `sdk-web` | `tsc` build + node tests of the Web SDK (sim tests load the wasm artifact; live tests skip without `NET_API_KEY`) |
| `sdk-csharp` | dotnet builds of the live/rehost checks and the managed test runner (P/Invokes the native artifact) |
| `console` | dashboard `check`/`lint`/`test`/`build`, then the live Playwright suite against a node started from the artifact with Postgres/Redis services (`AURIX_NET_E2E_TOKEN` = that node's `auth.admin_token`) |
| `docker` | buildx of the node image (`Dockerfile`) and the console image (`dashboard/Dockerfile`), no push |
