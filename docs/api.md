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
| POST | `/v1/operator/projects/:id/limits` | operator | quotas: `max_open_rooms, max_players_per_room, max_events_per_sec_per_room, max_event_bytes, regions` |
| GET | `/v1/operator/nodes` | operator | all nodes incl. draining |
| DELETE | `/v1/operator/nodes/:id` | operator | forget a node |

Errors: `{"error": {"code": "...", "message": "..."}}` with codes `unauthorized, forbidden,
not_found, conflict, invalid, room_full, room_closed, schema_mismatch, quota_exceeded,
rate_limited`.

## WebSocket control channel `/ws`

JSON messages, `"t"` is the type (snake_case). Client → server: `auth{grant}`,
`create{...}`, `join{name, player_props, rejoin_actor?, schema_hash?}`,
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
ack-only. Max datagram 1200 bytes, messages up to 64 KiB fragmented. Frames inside: relay
event frame or a mode frame (`docs/modes.md`).

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
