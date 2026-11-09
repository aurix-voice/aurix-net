# Aurix Net

Open-source game networking service: rooms and relay, server-authoritative state sync, deterministic lockstep. Companion of Aurix Voice; separate server, database and client. One server, one client, three room modes (`relay`, `sync`, `lockstep`). Priced by node-hours, never by CCU.

Status: 0.1.0 release candidate — relay, sync and lockstep rooms, the lockstep referee, matchmaking, aurix-sim (deterministic 2D/3D physics, navmesh, bots) and the SDKs for Web, Unity, Rust, C/C++ and Godot are verified locally, incl. a clean-checkout replay of every CI job ([CHANGELOG.md](CHANGELOG.md), [docs/testing.md](docs/testing.md)); what still needs real infrastructure is in [docs/roadmap.md](docs/roadmap.md). See `docs/modes.md`, `docs/api.md` and `docs/sdk.md`.

## Quickstart

```bash
docker compose -f deploy/docker-compose.dev.yml up -d
cargo build --release
NET__AUTH__ADMIN_TOKEN=change-me ./target/release/aurix-net-server
# create a project (returns the first API key)
curl -s -X POST localhost:8090/v1/operator/projects -H 'Authorization: Bearer change-me' \
     -H 'content-type: application/json' -d '{"name":"demo"}'
# load test: 100 rooms x 10 players, 20 events/s each, over QUIC
./target/release/aurix-net-loadtest --api-key an_... --rooms 100 --players 10 --rate 20
```

Config: TOML file via `--config`, every key overridable as `NET__SECTION__KEY` (see `crates/net-common/src/config.rs`).

Two roles run it: the **operator** (whoever hosts the nodes — sets quotas, drains, suspends, bills) and
**projects** (API keys scoped to one game). Self-hosting means you are both; see
[docs/api.md](docs/api.md#who-does-what-operator-vs-project).

## Layout

| Crate | Role |
|---|---|
| `net-common` | ids, errors, config, protocol limits |
| `net-transport` | 12-byte datagram header, channels (unreliable-sequenced, reliable-ordered, reliable-unordered) with own acks/retransmits, fragmentation, batching, QUIC endpoints (pinned certs), data tickets |
| `net-auth` | API keys (SHA-256 at rest), join grants (JWT HS256), operator token |
| `net-db` | Postgres (sqlx), migrations in `migrations/` |
| `net-control` | node registry/placement, project quotas, Redis live room directory + lobby pub/sub |
| `net-rooms` | relay room logic: membership, props with CAS, events/targets/groups/cache, master, kick, offline slots |
| `net-api` | WebSocket control channel, QUIC data plane, REST + operator API, metrics |
| `net-server` | the node binary `aurix-net-server` |
| `net-wire` | schema IDL, bit codec, quantization, deltas |
| `net-sim` | simulation frame: entities, components, inputs, snapshot/hash |
| `net-replication` | per-client baselines, AOI, priority, delta build/apply |
| `net-lagcomp` | hitbox history, rewind raycasts |
| `net-fpmath` | Q32.32 fixed point, deterministic trig and RNG |
| `net-rollback` | prediction, rollback, hashes, replays |
| `net-client` | Rust client (control + data path) |
| `net-ffi` | `libaurix_net` C ABI + `include/aurix_net.h` |
| `net-cli` | `aurix-net` command line |
| `net-loadtest` | bots: relay / sync / lockstep, loss/latency/tick report |

## Protocol in one paragraph

Clients open a WebSocket (`/ws`) for control (JSON), authenticate with a grant issued by the developer's backend (`POST /v1/grants` with an API key), and join a room. The server answers with a data endpoint (host, QUIC port, certificate fingerprint, 30-second ticket). The client connects over QUIC, sends the ticket as the first datagram, and from then on game events travel as datagrams on three channels; small unreliable messages are coalesced per datagram. Where UDP is blocked the same datagrams ride as binary WebSocket frames.

## License

Two licences, by what the code is for:

- **Server, AGPL-3.0-only** ([LICENSE](LICENSE)): `net-server`, `net-api`, `net-auth`, `net-control`, `net-db`, `net-rooms`, `net-game`, `net-cli`, `net-loadtest` and the `dashboard`. Run it, change it, self-host it; if you offer a modified node as a service, publish the changes.
- **Everything a game embeds, Apache-2.0** ([LICENSE-APACHE](LICENSE-APACHE)): `net-client`, `net-common`, `net-wire`, `net-transport`, `net-ffi`, `net-replication`, `net-rollback`, `net-lagcomp`, `net-fpmath`, all `net-sim*` crates and every SDK under `sdk/` (Web, Unity, Godot, C/C++). Ship them in closed-source games without obligations beyond attribution.

Each crate states its licence in `Cargo.toml`; each SDK package in its `package.json`.
