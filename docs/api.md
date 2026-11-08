# Aurix Net API

Base URL: a node's HTTP address (local default `http://127.0.0.1:8090`). Developer calls carry
`Authorization: Bearer an_<prefix>_<secret>` (an API key); operator calls carry the node's
operator token. Game clients never hold an API key — they receive a short-lived **grant**
(JWT) from your backend and present it on the WebSocket.

## Who does what: operator vs project

Two roles, the same in a self-hosted fleet and in a managed one:

- **Operator** — whoever runs the nodes (you, your team, or a cloud provider). Holds the config
  `admin_token` or a named operator token; sees every project, sets quotas and monthly caps,
  drains nodes, suspends projects, reads the fleet audit and billing report. Quotas exist to keep
  one project (or one bug) from eating the fleet; every default is "no limit".
- **Project** — a game team with an API key. Sees and manages only its own project: keys, webhooks,
  WASM modules, bans, matchmaking, usage, rejections and audit; reads its effective limits but
  cannot change them. Game clients hold neither — only short-lived grants.

Self-hosting means you are both; nothing in the operator role is locked behind a plan.

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
| GET/POST | `/v1/rooms` | `read`/`rooms` | list open rooms / create a **relay** room (`name, max_players, visible, open, props, lobby_props, reserved`); sync/lockstep rooms are created by a client through the control plane (`create`) because they need the schema/simulation setup |
| GET/DELETE | `/v1/rooms/:id` | `read`/`rooms` | details / close; `players[]` comes from the host node (any node answers) with live per-player counters `events`, `bytes_in`, `eps` (1 s window), `idle_secs`, `since_secs` — relay events raised through the data path; reset on rehost |
| GET | `/v1/rooms/:id/desyncs` | `read` | state dumps collected after lockstep desyncs (`{tick, actor, hash, bytes, at}`, 24 h) |
| GET | `/v1/rooms/:id/desyncs/:tick/:actor` | `read` | one dump as `application/octet-stream` — diff the desynced client's against the reference: actor `0` is the node's own referee state (rooms with a `logic` module), otherwise the master's |
| GET | `/v1/rooms/:id/replay` | `read` | lockstep input journal: stored 24 h after close, or live from the hosting node; `?from=&to=` cuts a tick window (instant replay / kill cam) |
| GET | `/v1/usage?format=csv` | read | same rows as CSV (`hour,metric,quantity`, attachment) for billing exports; also on `/v1/operator/usage` and `/v1/operator/projects/:id/usage` |
| GET | `/v1/usage` | `read` | node-hours, events, bytes per hour |
| GET/POST/DELETE | `/v1/keys[/:id]` | `admin` | API keys (plaintext shown once); `{name, scopes[], expires_in_secs?, allowed_cidrs?[]}` — with `allowed_cidrs` (CIDR or IP, ≤32) the key is accepted only from those networks (client IP after `trusted_proxies`), otherwise 403 and rejection `ip_denied` |
| GET/POST/DELETE | `/v1/webhooks[/:id]` | `admin` | webhooks (`url, events[]` — names from [webhooks.md](webhooks.md), empty = all), secret shown once |
| GET | `/v1/matchmaking` | read | live queue: `searching[]` (ticket, player, mode, region, max_players, age_secs) and `matched.last_hour` / `matched.last_24h` (rooms formed); operators: `GET /v1/operator/projects/:id/matchmaking` |
| POST | `/v1/matchmaking/tickets` | `rooms` | `{player_id, mode, region?, max_players, min_players, bots?, max_wait_secs, props, attrs?, filter?, fill_existing?, match_type?}` → `{ticket, state}` |
| GET/DELETE | `/v1/matchmaking/tickets/:id` | `read`/`rooms` | `state: searching{waiting} | matched{room, mode, max_players, players} | cancelled` |
| GET/POST | `/v1/operator/projects` | operator | projects; create returns the first key |
| POST | `/v1/operator/projects/:id/grants` | operator | test grant for a project (`player_id`, `name`, `room`, `scopes`, `ttl_secs` ≤ 3600, default 300); suspended/banned → 403; audited `grant.issue` (token never logged) |
| POST | `/v1/operator/projects/:id/keys` | operator | `{name, scopes, expires_in_secs?}` — expired keys fail auth like revoked ones; `expires_at` is returned in every key listing |
| POST | `/v1/operator/projects/:id/limits` | operator | quotas: `max_rooms, max_players_per_room, max_events_per_sec_per_room, max_events_per_sec_per_player, max_event_bytes, allowed_regions[]`, and calendar-month caps `monthly_player_minutes`, `monthly_bytes` (in + out): at ≥80 % the project shows under `/v1/operator/attention.usage_caps`, at 100 % new rooms are refused (`quota_exceeded`, rejection `usage_cap`) until the month rolls over; month-to-date totals in the project detail as `usage_month` |
| POST | `/v1/operator/projects/:id/suspend` | operator | `{suspended}`: while suspended the project's keys can only read; grants, joins, room creation and writes get 403 `project suspended`. Live rooms drain as players leave. |
| DELETE | `/v1/rooms/:id/players/:actor` | rooms | kick a player (`?reason=` ≤64 printable ASCII chars, shown to the client as `left{reason}`); works for rooms hosted on any node; audited as `player.kick` |
| DELETE | `/v1/operator/rooms/:id/players/:actor` | operator | same for operators |
| POST | `/v1/rooms/:id/notice` | rooms | push a system notice `{text (≤512), level: info|warn}` to every online player of the room (any node); clients get `{"t":"notice","text","level"}`; audited `room.notice` |
| POST | `/v1/notice` | rooms | same to every live room of the project on every node → `{rooms, players}`; audited `project.notice`; operators: `POST /v1/operator/rooms/:id/notice`, `POST /v1/operator/projects/:id/notice` |
| GET | `/v1/players/:player_id` | read | where a player is right now across every live node (`rooms[]` with room id/name/mode/node/actor), `ban` (or null), last 20 `audit` rows targeting them, `unreachable_nodes`; operators: `GET /v1/operator/projects/:id/players/:player_id` |
| GET/POST | `/v1/bans` | read / rooms | project deny-list; POST `{player_id, reason?, duration_secs?}` (omit duration = permanent). Banned players get 403 `banned…` at `/v1/grants` and at join; counted as rejection reason `banned` |
| DELETE | `/v1/bans/:player_id` | rooms | lift a ban |
| GET/POST/DELETE | `/v1/operator/projects/:id/bans[/:player_id]` | operator | same for operators |
| GET | `/v1/rejections` | read | why this project's players/keys were turned away in the last `?hours=` (≤26, default 24): hourly `series` + `total` by reason — `rate_limited`, `quota`, `usage_cap`, `ip_denied`, `capacity`, `room_full`, `suspended`, `banned`, `region`, `grant`. Counted on every node (Redis), 26 h retention |
| GET | `/v1/operator/projects/:id/rejections` | operator | same for operators |
| GET | `/v1/webhooks/:id/deliveries` | read | last deliveries of a webhook (`?limit=`, ≤500): event, ok, HTTP status, attempts, latency, error; kept `retention.webhook_deliveries_days` (7) |
| POST | `/v1/webhooks/:id/rotate` | admin | new signing secret (shown once), `{grace_secs?}` (default 24 h, max 7 d): until then deliveries also carry `X-Aurix-Signature-Previous` signed with the old secret; audited `webhook.rotate`; operators: `POST /v1/operator/projects/:id/webhooks/:wid/rotate` |
| POST | `/v1/webhooks/:id/test` | admin | queue a `test` event to that webhook only; result shows in its deliveries |
| GET/POST | `/v1/operator/projects/:id/webhooks/:wid/deliveries` · `/test` | operator | same for operators (test is audited as `webhook.test`) |
| DELETE | `/v1/operator/projects/:id` | operator | soft delete: live rooms closed fleet-wide, all keys revoked, webhooks/modules removed; usage and audit rows stay |
| GET/POST | `/v1/operator/tokens` | operator | named operator tokens: POST `{name, expires_in_secs?, allowed_cidrs?[]}` returns the plaintext once (with `allowed_cidrs` the token works only from those networks → 403 elsewhere); the list carries `last_used_at` / `last_used_ip`; they authenticate every operator route and appear in audit as actor `op:<name>` (the config `admin_token` stays actor `operator`) |
| GET | `/v1/operator/billing` | operator | month report for invoicing: `?month=YYYY-MM` (default current, UTC) → per project `player_minutes, room_minutes, bytes_in, bytes_out, rooms_created`, the caps that applied, `deleted_at`; `totals`; `?format=csv` for a spreadsheet. Soft-deleted projects stay in the month they were used |
| DELETE | `/v1/operator/tokens/:id` | operator | revoke (audited `operator_token.revoke`) |
| GET/POST/DELETE | `/v1/operator/webhooks[/:id]` · `GET /:id/deliveries` · `POST /:id/test` | operator | fleet alert webhooks (`{url, events[]}`, secret shown once; events in [webhooks.md](webhooks.md#operator-alerts-fleet-level)); audited `operator_webhook.create/delete/test` |
| GET | `/v1/audit?format=csv` · `/v1/operator/audit?format=csv` | read / operator | same filters (`action`, `actor`, `limit`, operator `project`) as CSV attachment: id, created_at, project_id, actor, action, target, details (JSON) |
| GET | `/v1/operator/attention` | operator | what needs a look now: `stale_nodes` (no heartbeat > 30 s), `draining_nodes`, `suspended_projects`, `failing_webhooks` (failed deliveries in the last hour), `rejections` (projects with rejections this hour, by reason) |
| GET | `/v1/operator/nodes` | operator | all nodes incl. draining |
| GET | `/v1/operator/nodes/:id/stats` | operator | live process stats of one node (answered by that node): uptime, rooms by mode, players/spectators, control sessions, datagram/byte counters, sync tick avg/p99 ms, overruns, lost deltas, resyncs, desyncs, adopted rooms, RSS |
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
`set_groups{join, leave}`, `cache_slice{op, index?}` (relay event-cache slices), `set_view{viewpoints, interest}` (sync AOI: extra viewpoints, forced entities),
`set_master{actor}`, `kick{actor, reason}`, `lobby{subscribe}`,
`stats` (live rooms/players per lobby), `find_friends{players}` (presence of ≤ 64 players; `stats`/`find_friends` at most once per second per session),
`rpc{id, name, body}` (call your backend through the node, see Webhooks; body ≤ 8 KiB, 5 per second per session), `ping{ts}`.

Server → client: `hello{session, version, region}`, `authed{player_id, name}`,
`joined{room, name, mode, actor, master, max_players, props, players, data, cached, schema_hash?, tick_hz?}`,
`left{reason}`, `player_joined{player}`, `player_left{actor, reason}`, `room_props`,
`player_props`, `event{code, from, data}` (control fallback), `master_changed{actor}`,
`lobby_rooms{rooms}`, `lobby_update{room, removed}`, `stats{rooms, players, lobbies[{lobby, rooms, players}]}`,
`friends{friends[{player, online, room?{id, name, mode, node}}]}`, `rpc_result{id, ok, status?, body, error?}`, `pong{ts, server_ms}`,
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

Two events are synchronous calls rather than notifications: `join.validate` (the receiver
answers `{"allow": bool, "reason"?}` within 2 s; see Keys, grants, scopes) and `rpc` — a
client's `rpc{name, body}` is POSTed as `data: {name, player_id, actor?, room?, body}` to the
first webhook subscribed to `rpc`, which has 5 s to answer up to 16 KiB of JSON; the status and
body go back to that client as `rpc_result` (WebRPC: leaderboards, inventory, purchases
without exposing your backend's URL or auth to the client — the node signs every call). One
attempt, no retries.

## Matchmaking flow

1. Backend (or client with a `rooms` key through your backend) files a ticket.
2. Poll the ticket every 0.5–1 s until `matched`.
3. Client `join_or_create` with `name = state.room`, the same `mode`, `max_players`.
Tickets group by `(mode, region, max_players, props)`; a match starts when the group is full
or the oldest ticket waited `max_wait_secs` with at least `min_players`.

Skill/attribute matching: a ticket may carry `attrs` (≤ 1 KiB of JSON: `{"rank": 1450, "lang": "en"}`) and a
`filter` — a SQL-like condition the *other* players' attrs must satisfy, e.g.
`rank >= 1300 AND rank <= 1600 AND lang = 'en'`. Two tickets share a room only when each filter accepts
the other's attrs (so a "picky" player never lands with someone who would reject them). Grammar:
`attr op value` with `= != <> < <= > >=`, numbers / 'strings' / true|false, `AND`, `OR`, `NOT`, parentheses;
≤ 256 chars, ≤ 16 comparisons; a missing attribute fails every comparison. Bad filters are rejected with 400
at filing. `props` still bucket exactly (map, game mode) — use `attrs` for ranges. To widen a search over time,
re-file the ticket with a looser filter after `max_wait_secs`.

Filling running rooms: `fill_existing: true` seats the ticket in an already running open, visible room of the
same mode with a free slot whose lobby props satisfy `filter` (a random join with an SQL-lobby filter
filter), before waiting for a fresh group; `match_type` chooses among candidates — `fill` (fullest first,
default), `serial` (emptiest first) or `random`. The result is `matched` with `players: [you]`. Clients can
also do this directly on the control plane: `{"t":"join_random","filter":{..exact props..},"expr":"map = 'dust'",
"match_type":"fill"}` — the node answers with its own matching room or a `room_elsewhere:<url>#<room>`
redirect to the node that hosts one.

Bot backfill: `bots: true` on a ticket means "if `max_wait_secs` runs out short-handed, start anyway
and report the empty seats". When every member of a group agreed, the ticket state and the
`match.found` webhook carry `bots: <seats>`; the game (room master or WASM logic) spawns that many
AI players — the service never fakes players itself.

Party slots: `reserved: ["p2","p3"]` on room creation (REST, `create`, `join_or_create`) holds those seats
for 60 s — other players see the room as full until the party has arrived or the hold lapses. Matchmaking
results carry the `players` list; pass it as `reserved` when the first of them creates the room.

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
| Room name | 1..=64 chars, no control characters | `invalid` |
| `lobby_props` per room | ≤32 keys, each 1..=64 chars | `invalid` |
| Control message (post-auth) | 68 KiB; 8 KiB before `auth` | connection closed |
| Write calls per API key (grants, rooms, tickets, keys, webhooks, modules) | 20/s | 429 `rate_limited` |
| Pre-auth control message | 8 KiB, 10 s to authenticate | connection closed |
| Request body | 64 KiB (module upload 8 MiB) | 413 |

`GET /healthz` answers 503 `{"ok":false,"draining":true}` while the node shuts down; players
in its rooms get `left{reason:"rehost"}` and rejoin the same room name on any node.
`GET /metrics` requires `Authorization: Bearer <server.metrics_token>` when that is set.

## Audit log

Every admin/operator mutation writes a row to the `audit` table (`actor` is `key:<api key id>` or
`operator`): `project.create`, `project.limits`, `project.suspend`, `project.resume`, `project.delete`, `node.drain`, `node.undrain`, `room.close`, `webhook.test`, `player.kick`, `player.ban`, `player.unban`, `webhook.rotate`, `operator_token.create`, `operator_token.revoke`, `room.notice`, `project.notice`, `grant.issue`, `key.create`, `key.revoke`, `webhook.create`,
`webhook.delete`, `module.put`, `module.delete`. Rows are kept for `retention.audit_days` (90).
