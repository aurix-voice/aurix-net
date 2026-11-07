# Aurix Net API

Base URL: a node's HTTP address (local default `http://127.0.0.1:8090`). Developer calls carry
`Authorization: Bearer an_<prefix>_<secret>` (an API key); operator calls carry the node's
operator token. Game clients never hold an API key — they receive a short-lived **grant**
(JWT) from your backend and present it on the WebSocket.

## Keys, grants, scopes

| Scope | Allows |
|---|---|
| `read` | list/get rooms, regions, usage, tickets |
| `rooms` | create/close rooms, matchmaking tickets |
| `grants` | `POST /v1/grants` |
| `admin` | keys, webhooks |

`POST /v1/grants` `{player_id, name, room?, scopes?, meta?, ttl_secs?}` → `{grant, expires_at}`.
The grant is bound to the project; `room` restricts the player to one room name.

## REST

| Method | Path | Auth | Notes |
|---|---|---|---|
| GET | `/healthz` | none | node id, region, rooms, players |
| GET | `/metrics` | none | Prometheus |
| GET | `/v1/regions` | key | regions with live nodes |
| GET | `/v1/nodes` | key | nodes visible to the project |
| POST | `/v1/grants` | `grants` | issue a join grant |
| GET/POST | `/v1/rooms` | `read`/`rooms` | list open rooms / create (`name, mode, max_players, visible, open, props, lobby_props, region`) |
| GET/DELETE | `/v1/rooms/:id` | `read`/`rooms` | details / close |
| GET | `/v1/rooms/:id/replay` | `read` | lockstep input journal (24 h after close) |
| GET | `/v1/usage` | `read` | node-hours, events, bytes per hour |
| GET/POST/DELETE | `/v1/keys[/:id]` | `admin` | API keys (plaintext shown once) |
| GET/POST/DELETE | `/v1/webhooks[/:id]` | `admin` | webhooks (`url, events[]`), secret shown once |
| POST | `/v1/matchmaking/tickets` | `rooms` | `{player_id, mode, region?, max_players, min_players, max_wait_secs, props}` → `{ticket, state}` |
| GET/DELETE | `/v1/matchmaking/tickets/:id` | `read`/`rooms` | `state: searching{waiting} | matched{room, mode, max_players, players} | cancelled` |
| GET/POST | `/v1/operator/projects` | operator | projects; create returns the first key |
| POST | `/v1/operator/projects/:id/keys` | operator | |
| POST | `/v1/operator/projects/:id/limits` | operator | quotas: `max_open_rooms, max_players_per_room, max_events_per_sec_per_room, max_events_per_sec_per_player, max_player_props_bytes, max_event_bytes, regions` |
| POST | `/v1/operator/projects/:id/suspend` | operator | `{suspended}`: while suspended the project's keys can only read; grants, joins, room creation and writes get 403 `project suspended`. Live rooms drain as players leave. |
| DELETE | `/v1/operator/projects/:id` | operator | soft delete: live rooms closed fleet-wide, all keys revoked, webhooks/modules removed; usage and audit rows stay |
| GET | `/v1/operator/nodes` | operator | all nodes incl. draining |
| GET | `/v1/internal/rooms/:id/players` | operator | node-to-node: players of a room hosted on this node (used by room detail on other nodes) |
| DELETE | `/v1/internal/rooms/:id` | operator | node-to-node: close a room hosted on this node |
| POST | `/v1/internal/drain` | operator | node-to-node: drain/undrain this node |
| POST | `/v1/operator/nodes/:id/drain` | operator | `{draining}`: proxied to the target node; `true` refuses joins, hands every room off through checkpoints and answers 503 on `/healthz` without stopping the process; `false` puts it back in rotation |
| DELETE | `/v1/operator/nodes/:id` | operator | forget a node |

Errors: `{"error": {"code": "...", "message": "..."}}` with codes `unauthorized, forbidden,
not_found, conflict, invalid, room_full, room_closed, schema_mismatch, quota_exceeded,
rate_limited`.

## WebSocket control channel `/ws`

JSON messages, `"t"` is the type (snake_case). Client → server: `auth{grant}`,
`create{...}`, `join{name, player_props, rejoin_actor?, schema_hash?, spectator?}`,
`join_or_create{name, mode, max_players, props, lobby_props, region, player_props}`,
`join_random{...}`, `leave`, `set_room_props{props, expected?}`,
`set_player_props{props, expected?}`, `event{code, data(base64), target, reliable, cache, group}`,
`set_groups{join, leave}`, `set_master{actor}`, `kick{actor, reason}`, `lobby{subscribe}`,
`ping{ts}`.

Server → client: `hello{session, version, region}`, `authed{player_id, name}`,
`joined{room, name, mode, actor, master, max_players, props, players, data, cached, schema_hash?, tick_hz?}`,
`left{reason}`, `player_joined{player}`, `player_left{actor, reason}`, `room_props`,
`player_props`, `event{code, from, data}` (control fallback), `master_changed{actor}`,
`lobby_rooms{rooms}`, `lobby_update{room, removed}`, `pong{ts, server_ms}`,
`error{code, message}`, `desync{tick, actor}`.

`joined.data` = `{host, port, cert_sha256, ticket(base64), ws_tunnel, webtransport_url?}`: connect QUIC
(ALPN `aurix-net/1`, pin `cert_sha256`), send the ticket as the first datagram, wait for the
empty reliable frame. Where UDP is blocked send the ticket as the first binary WebSocket
frame and use binary frames as datagrams.

## Data path

12-byte header: `u8 version, u8 kind, u16 seq, u16 ack, u32 ack_bits, u16 tick`. `kind`
low bits = channel (0 unreliable-sequenced, 1 reliable-ordered, 2 reliable-unordered) and
fragment flags; `0x40` has-ack, `0x20` batch (payload = `u16 len, bytes` repeated), `0x80`
ack-only. Max datagram 1100 bytes, messages up to 64 KiB fragmented. Frames inside: relay
event frame or a mode frame (`docs/modes.md`).

### WebTransport (browsers)

Nodes also listen for WebTransport sessions (`transport.webtransport_addr`, default
`0.0.0.0:7443`; `joined.data.webtransport_url` = `https://host:7443/`). The protocol is the
same as raw QUIC: the first datagram is the ticket, then packets flow as HTTP/3 datagrams
through the same link/channels. Browsers pin the certificate via `serverCertificateHashes`
(= `cert_sha256`), so the self-signed dev certificate works; it is valid 13 days and
regenerated on every start. The WebSocket tunnel remains the fallback. Manual browser check:
`sdk/web/test/browser.html` (two clients, echo over WebTransport).

## Metrics (`GET /metrics`, Prometheus)

| Metric | Meaning |
|---|---|
| `net_rooms`, `net_players`, `net_control_sessions` | hosted rooms, online players, control connections |
| `net_datagrams_in/out_total`, `net_bytes_in/out_total` | data path volume |
| `net_events_total{channel}` | relayed events / deltas / bundles |
| `net_errors_total{code}` | client-visible errors by code |
| `net_sync_tick_seconds` | sync/lockstep tick duration histogram |
| `net_tick_overruns_total` | ticks longer than the tick period |
| `net_sync_lost_deltas_total` | deltas clients lost (re-sent in full) |
| `net_sync_resyncs_total` | full-snapshot requests |
| `net_sync_aoi_starved_total` | client-ticks where the byte budget deferred entities |
| `net_sync_inputs_rate_limited_total`, `net_sync_invalid_inputs_total` | dropped input frames |
| `net_lockstep_desyncs_total` | hash reports disagreeing with the majority |
| `net_rooms_adopted_total` | rooms taken over from a dead node |

## Usage metering

Every node adds per project and hour: `room_minutes`, `player_minutes`, `bytes_in`,
`bytes_out` (`GET /v1/usage?from&to`). Billing is node-hours plus included traffic, never
peak CCU. `GET /v1/regions` lists live regions with `endpoints` (node HTTP URLs) so clients
pick the closest node; a room lookup on another node answers `conflict room_elsewhere:<url>`.
`Client::connect_and_join` (Rust) and `Client.connectAndJoin` (Web) follow that redirect
automatically; the C ABI returns the error and the caller reconnects to the URL.

## Webhooks

POST JSON `{event, project, at, data}` with `X-Aurix-Event` and
`X-Aurix-Signature: sha256=<hex HMAC-SHA256(secret, body)>`. Events: `room.created
{room, name, mode, region}`, `room.closed {room}`, `player.joined {room, actor}`,
`player.left {room, actor}`, `desync {room, tick, actor}`, `room.rehosted {room, name, mode, node, restored}` (room adopted by another node after a node failure). Three attempts; 4xx stops retries.

## Matchmaking flow

1. Backend (or client with a `rooms` key through your backend) files a ticket.
2. Poll the ticket every 0.5–1 s until `matched`.
3. Client `join_or_create` with `name = state.room`, the same `mode`, `max_players`.
Tickets group by `(mode, region, max_players, props)`; a match starts when the group is full
or the oldest ticket waited `max_wait_secs` with at least `min_players`.

Tickets live in Redis (`net:mm:*`), so a ticket filed on any node of the region matches
tickets filed on every other node and `GET` works from any node; one node at a time runs the
matching pass behind a 1.5 s leader lock, so a dead node is replaced within a cycle.
Searching tickets expire after 5 minutes, results stay readable for 2 minutes.

## CLI

`aurix-net` (crate `net-cli`): `health, regions, project-create, projects, nodes, key-create,
keys, grant, rooms, room, room-close, replay -o file, match, usage, webhooks, webhook-create`.

## Game modules (server-side logic, sync rooms)

Authoritative game logic runs on the node inside a WebAssembly sandbox — on every plan, no
dedicated server. Upload a module per project, reference it from room props.

| | |
|---|---|
| `PUT /v1/modules/:name` (admin) | body = `.wasm` binary or WAT text, <= 8 MiB; compiled and validated (`memory`, `aurix_init`, `aurix_step` exports); returns `{name, hash, size}` |
| `GET /v1/modules` (read) | `{modules: [{name, hash, size, created_at, updated_at}]}` |
| `DELETE /v1/modules/:name` (admin) | 204 |

Room props: `{"schema": "<IDL>", "logic": "<name>", "position": "Transform.0"}`. The host
ABI (imports under module `aurix`: spawn/despawn, component get/set by id and field index,
inputs, `raise` events, `hitbox` for lag compensation, `log`) is documented in
`crates/net-game/src/lib.rs`; a complete guest is `crates/net-game/examples/arena.wat`.
Limits per room: 50M fuel per call, 64 MiB memory; a trap disables the module for that room
and the room keeps running. Guest memory is not checkpointed — keep state in components.
CLI: `aurix-net modules | module-put <name> <file> | module-delete <name>`.

## Limits and errors

| Limit | Default | Error |
|---|---|---|
| HTTP requests per IP | 50/s (burst 100) | 429 |
| WebSocket connects per IP | 60/min | 429 |
| Concurrent sessions/data connections per IP | 64 | 429 / connection refused |
| Failed `auth` per IP | 20/min | `rate_limited`, connection closed |
| Write calls per API key (grants, rooms, tickets, keys, webhooks, modules) | 20/s | 429 `rate_limited` |
| Pre-auth control message | 8 KiB, 10 s to authenticate | connection closed |
| Request body | 64 KiB (module upload 8 MiB) | 413 |

`GET /healthz` answers 503 `{"ok":false,"draining":true}` while the node shuts down; players
in its rooms get `left{reason:"rehost"}` and rejoin the same room name on any node.
`GET /metrics` requires `Authorization: Bearer <server.metrics_token>` when that is set.

## Audit log

Every admin/operator mutation writes a row to the `audit` table (`actor` is `key:<api key id>` or
`operator`): `project.create`, `project.limits`, `project.suspend`, `project.resume`, `project.delete`, `node.drain`, `node.undrain`, `room.close`, `key.create`, `key.revoke`, `webhook.create`,
`webhook.delete`, `module.put`, `module.delete`. Rows are kept for `retention.audit_days` (90).
