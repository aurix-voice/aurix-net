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
| `rooms` | create/close rooms, matchmaking tickets, `POST /v1/grants` |
| `admin` | keys, webhooks |

`POST /v1/grants` `{player_id, name, room?, scopes?, meta?, ttl_secs?}` → `{grant, expires_at}`.
The grant is bound to the project; `room` restricts the player to one room name.
`player_id` is 1 to 128 characters, none of them a control character, wherever the API takes
one: grants, bans, matchmaking tickets, `reserved`.

## REST

| Method | Path | Auth | Notes |
|---|---|---|---|
| GET | `/healthz` | none | node id, region, rooms, players |
| GET | `/metrics` | none | Prometheus |
| GET | `/v1/me` | key or operator | who the bearer is: the operator, or an API key with its project and scopes; the console signs in with it. A key of any scopes may ask; a revoked or expired one gets 401, one used from an address outside its `allowed_cidrs` 403 |
| GET | `/v1/regions` | none | regions with live nodes; the SDKs read it to pick the closest one |
| GET | `/v1/nodes` | `read` | the live nodes: id, region, host, QUIC port, URL, certificate hash, load |
| POST | `/v1/grants` | `rooms` | issue a join grant; `ttl_secs` is at least 1 and is cut to the node's `auth.grant_ttl_secs`, 0 is refused with 400 |
| GET/POST | `/v1/rooms` | `read`/`rooms` | list open rooms / create a **relay** room (`name, max_players, visible, open, props, lobby_props, reserved`); sync/lockstep rooms are created by a client through the control plane (`create`) because they need the schema/simulation setup |
| GET/DELETE | `/v1/rooms/:id` | `read`/`rooms` | details / close (500 `internal` and the room stays if the node hosting it could not be asked); `players[]` comes from the host node (any node answers) with live per-player counters `events`, `bytes_in`, `eps` (1 s window), `idle_secs`, `since_secs` — relay events raised through the data path; reset on rehost |
| GET | `/v1/rooms/:id/desyncs` | `read` | state dumps collected after lockstep desyncs (`{tick, actor, hash, bytes, at}`, 24 h) |
| GET | `/v1/rooms/:id/desyncs/:tick/:actor` | `read` | one dump as `application/octet-stream` — diff the desynced client's against the reference: actor `0` is the node's own referee state (rooms with a `logic` module), otherwise the master's |
| GET | `/v1/rooms/:id/replay` | `read` | lockstep input journal: stored 24 h after close, or live from the hosting node; `?from=&to=` cuts a tick window (instant replay / kill cam) |
| GET | `/v1/usage?format=csv` | read | same rows as CSV (`hour,metric,quantity`, attachment) for billing exports; also on `/v1/operator/usage` and `/v1/operator/projects/:id/usage` |
| GET | `/v1/usage` | `read` | node-hours, events, bytes per hour |
| GET/POST/DELETE | `/v1/keys[/:id]` | `admin` | API keys (plaintext shown once); `{name, scopes[], expires_in_secs?, allowed_cidrs?[]}` — with `allowed_cidrs` (CIDR or IP, ≤32) the key is accepted only from those networks (client IP after `trusted_proxies`), otherwise 403 and rejection `ip_denied` |
| GET/POST/DELETE | `/v1/webhooks[/:id]` | `admin` | webhooks (`url, events[]` — names from [webhooks.md](webhooks.md), empty = all), secret shown once |
| GET | `/v1/matchmaking` | read | live queue: `searching[]` (ticket, player, mode, region, max_players, age_secs) and `matched.last_hour` / `matched.last_24h` (rooms formed); operators: `GET /v1/operator/projects/:id/matchmaking` |
| POST | `/v1/matchmaking/tickets` | `rooms` | `{player_id, mode, region?, max_players, min_players, bots?, max_wait_secs, props, attrs?, filter?, fill_existing?, match_type?}` → `{ticket, state}` |
| GET/DELETE | `/v1/matchmaking/tickets/:id` | `read`/`rooms` | `state: searching{waiting} | matched{room, mode, max_players, players} | cancelled`; `DELETE` cancels a searching ticket and answers 204, also for one already cancelled; a matched one answers 409 `conflict` and stays matched |
| GET/POST | `/v1/operator/projects` | operator | projects; create returns the first key |
| POST | `/v1/operator/projects/:id/grants` | operator | test grant for a project (`player_id`, `name`, `room`, `scopes`, `ttl_secs` ≤ 3600, default 300); suspended/banned → 403; audited `grant.issue` (token never logged) |
| POST | `/v1/operator/projects/:id/keys` | operator | `{name, scopes, expires_in_secs?}` — expired keys fail auth like revoked ones; `expires_at` is returned in every key listing |
| POST | `/v1/operator/projects/:id/limits` | operator | quotas: `max_rooms, max_players_per_room, max_events_per_sec_per_room, max_events_per_sec_per_player, max_event_bytes, allowed_regions[], max_concurrent_players` (fleet-wide players online at once; a join over it is refused with `capacity`), and calendar-month caps `monthly_player_minutes`, `monthly_bytes` (in + out): at ≥80 % the project shows under `/v1/operator/attention.usage_caps`, at 100 % new rooms are refused (`quota_exceeded`, rejection `usage_cap`) until the month rolls over; month-to-date totals in the project detail as `usage_month`. The body replaces the project's limits; an unknown field or a value of the wrong type is refused with `invalid` and nothing changes. A project whose stored limits do not parse is not treated as unlimited: new rooms and joins are refused with `quota_exceeded` until an operator sets the limits again; it is listed under `/v1/operator/attention.invalid_limits` and its detail carries `limits_error` |
| POST | `/v1/operator/projects/:id/suspend` | operator | `{suspended}`: while suspended the project's keys can only read; grants, joins, room creation and writes get 403 `project suspended`. Live rooms drain as players leave. |
| DELETE | `/v1/rooms/:id/players/:actor` | rooms | kick a player (`?reason=` ≤64 printable ASCII chars, shown to the client as `left{reason}`); works for rooms hosted on any node; audited as `player.kick` |
| DELETE | `/v1/operator/rooms/:id/players/:actor` | operator | same for operators |
| POST | `/v1/rooms/:id/notice` | rooms | push a system notice `{text (≤512), level: info|warn}` to every online player of the room (any node); clients get `{"t":"notice","text","level"}`; audited `room.notice` |
| POST | `/v1/notice` | rooms | same to every live room of the project on every node → `{rooms, players}`; audited `project.notice`; operators: `POST /v1/operator/rooms/:id/notice`, `POST /v1/operator/projects/:id/notice` |
| GET | `/v1/players/:player_id` | read | where a player is right now across every live node (`rooms[]` with room id/name/mode/node/actor), `ban` (or null), last 20 `audit` rows targeting them, `unreachable_nodes`; operators: `GET /v1/operator/projects/:id/players/:player_id` |
| GET/POST | `/v1/bans` | read / rooms | project deny-list; POST `{player_id, scope?, reason?, duration_secs?}` (omit duration = permanent, `duration_secs: 0` is refused with 400; `scope` is `player` (default), `player_ip`, `player_device` or `player_ip_device` and also bans the IP and/or device the player was last seen with). Banned players get 403 `banned…` at `/v1/grants` and at join; counted as rejection reason `banned` |
| DELETE | `/v1/bans/:player_id` | rooms | lift a ban |
| DELETE | `/v1/bans/marks/:kind/:value` | rooms | lift one IP or device ban (`kind` is `ip` or `device`) left by a ban with `scope` |
| GET | `/v1/players/:player_id/export` | admin | everything stored about a player, as JSON; operators: `GET /v1/operator/projects/:id/players/:player_id/export` |
| DELETE | `/v1/players/:player_id/data` | admin | forget a player: identity, IP and device bans and the ban go, audit rows lose the target; operators: `DELETE /v1/operator/projects/:id/players/:player_id/data` |
| GET/POST/DELETE | `/v1/operator/projects/:id/bans[/:player_id]` | operator | same for operators |
| GET | `/v1/rejections` | read | why this project's players/keys were turned away in the last `?hours=` (≤26, default 24): hourly `series` + `total` by reason — `rate_limited`, `quota`, `usage_cap`, `ip_denied`, `capacity`, `room_full`, `suspended`, `banned`, `region`, `grant`. Counted on every node (Redis), 26 h retention |
| GET | `/v1/operator/projects/:id/rejections` | operator | same for operators |
| GET | `/v1/webhooks/:id/deliveries` | read | last deliveries of a webhook (`?limit=`, ≤500): event, ok, HTTP status, attempts, latency, error; kept `retention.webhook_deliveries_days` (7) |
| POST | `/v1/webhooks/:id/rotate` | admin | new signing secret (shown once), `{grace_secs?}` (default 24 h, max 7 d): until then deliveries also carry `X-Aurix-Signature-Previous` signed with the old secret; audited `webhook.rotate`; operators: `POST /v1/operator/projects/:id/webhooks/:wid/rotate` |
| POST | `/v1/webhooks/:id/test` | admin | queue a `test` event to that webhook only; result shows in its deliveries |
| GET/POST | `/v1/operator/projects/:id/webhooks/:wid/deliveries` · `/test` | operator | same for operators (test is audited as `webhook.test`) |
| DELETE | `/v1/operator/projects/:id` | operator | soft delete: live rooms closed fleet-wide, all keys revoked, webhooks/modules removed; usage and audit rows stay. `closed_rooms` counts the rooms closed by the time of the answer; a room whose node could not be asked is closed by that node with its next heartbeat |
| GET/POST | `/v1/operator/tokens` | operator | named operator tokens: POST `{name, expires_in_secs?, allowed_cidrs?[]}` returns the plaintext once (with `allowed_cidrs` the token works only from those networks → 403 elsewhere); the list carries `last_used_at` / `last_used_ip`; they authenticate every operator route and appear in audit as actor `op:<name>` (the config `admin_token` stays actor `operator`) |
| GET | `/v1/operator/billing` | operator | month report for invoicing: `?month=YYYY-MM` (default current, UTC) → per project `player_minutes, room_minutes, bytes_in, bytes_out, rooms_created`, the caps that applied, `deleted_at`; `totals`; `?format=csv` for a spreadsheet. Soft-deleted projects stay in the month they were used |
| DELETE | `/v1/operator/tokens/:id` | operator | revoke (audited `operator_token.revoke`) |
| GET/POST/DELETE | `/v1/operator/webhooks[/:id]` · `GET /:id/deliveries` · `POST /:id/test` | operator | fleet alert webhooks (`{url, events[]}`, secret shown once; events in [webhooks.md](webhooks.md#operator-alerts-fleet-level)); audited `operator_webhook.create/delete/test` |
| GET | `/v1/audit?format=csv` · `/v1/operator/audit?format=csv` | read / operator | same filters (`action`, `actor`, `limit`, operator `project`) as CSV attachment: id, created_at, project_id, actor, action, target, details (JSON) |
| GET | `/v1/operator/attention` | operator | what needs a look now: `stale_nodes` (two of the node's own heartbeats missed), `draining_nodes`, `suspended_projects`, `failing_webhooks` (failed deliveries in the last hour), `rejections` (projects with rejections this hour, by reason), `usage_caps` (projects at 80 % of a monthly cap or more), `invalid_limits` (projects whose stored limits do not parse, with the `error`) |
| GET | `/v1/operator/nodes` | operator | all nodes incl. draining |
| GET | `/v1/operator/rooms` | operator | every live room in the region, across projects; one project: `GET /v1/operator/projects/:id/rooms` |
| GET | `/v1/operator/projects/:id/audit` | operator | audit trail of one project, same filters as `/v1/operator/audit` |
| GET | `/v1/operator/nodes/:id/stats` | operator | live process stats of one node (answered by that node): uptime, rooms by mode, players/spectators, control sessions, datagram/byte counters, sync tick avg/p99 ms, overruns, lost deltas, resyncs, desyncs, adopted rooms, RSS |
| GET | `/v1/internal/rooms/:id/players` | operator | node-to-node: players of a room hosted on this node (used by room detail on other nodes) |
| DELETE | `/v1/internal/rooms/:id` | operator | node-to-node: close a room hosted on this node |
| POST | `/v1/internal/drain` | operator | node-to-node: drain/undrain this node |
| POST | `/v1/operator/nodes/:id/drain` | operator | `{draining}`: proxied to the target node; `true` refuses joins, hands every room off through checkpoints and answers 503 on `/healthz` without stopping the process; `false` puts it back in rotation |

Errors: `{"error": "<code>", "message": "..."}` with codes `unauthorized, forbidden,
not_found, conflict, invalid_request, room_full, room_closed, schema_mismatch, quota_exceeded,
rate_limited`; a failure inside the node answers 500 with `internal`, `database`, `redis`,
`transport` or `config`. For `database`, `redis` and `internal` the `message` says only which
of them failed, here and in `error` on the control channel; the cause is in the node's log,
with the route or the message type and the project. A request refused before its handler
has the same body: one that does not parse (400, 415, 422), is too large (413) or names a
method the path does not take (405) is `invalid_request`, an unknown path is `not_found`.

## WebSocket control channel `/ws`

JSON messages, `"t"` is the type (snake_case). Client → server: `auth{grant, supports?}`,
`create{name?, mode, max_players, visible, open, props, lobby_props, region, player_props, reserved, id?}`,
`join{name, player_props, rejoin_actor?, schema_hash?, spectator?, id?}`,
`join_or_create{name, mode, max_players, visible, open, props, lobby_props, region, player_props, reserved, id?}`,
`join_random{filter, player_props, expr?, match_type?, id?}`, `leave{id?}`, `set_room_props{props, expected?, visible?, open?}`,
`set_player_props{props, expected?}`, `event{code, data(base64), target, channel, cache, group}` (`target` is `{"kind": "all" | "others" | "master"}` or `{"kind": "actors", "actors": [..]}`, default others; `channel` is `unreliable_sequenced`, `reliable_ordered` (default), `reliable_unordered` or `unreliable_unsequenced`),
`set_groups{join, leave}`, `cache_slice{op, index?}` (relay event-cache slices), `set_view{viewpoints, interest}` (sync AOI: extra viewpoints, forced entities),
`set_master{actor}`, `kick{actor, reason}`, `lobby{subscribe}`,
`stats{id?}` (live rooms/players per lobby), `find_friends{players, id?}` (presence of ≤ 64 players; `stats`/`find_friends` at most once per second per session),
`get_rooms{expr?, limit?, id?}` (one-shot filtered room list, SQL-like `expr` on lobby props as in `join_random`; same one-per-second budget), `rpc{id, name, body}` (call your backend through the node, see Webhooks; body ≤ 8 KiB, 5 per second per session), `ping{ts}`.

The node closes a control connection it has heard nothing from for 90 s (10 s before `auth`);
any message counts, so a client that sends `ping` stays connected.

Server → client: `hello{session, version, region, supports?}`, `authed{player_id, name}`,
`joined{room, name, mode, actor, master, max_players, props, players, data, cached, schema_hash?, tick_hz?, next_spawn_tag?, id?}`,
`left{reason, room?, id?}`, `player_joined{player}`, `player_left{actor, reason}`, `room_props`,
`player_props`, `event{code, from, data}` (control fallback), `master_changed{actor}`,
`lobby_rooms{rooms}` (the whole list, in place of the one the client holds: sent on `lobby{subscribe: true}` and again after the node lost its Redis connection and may have missed changes), `lobby_update{room, removed}` (with `removed: true` the room is out of the list this client holds, closed or no longer offered to him, and `room` carries its `id` alone; it comes once, and not for a room the list never offered him), `stats{rooms, players, lobbies[{lobby, rooms, players}], id?}`,
`friends{friends[{player, online, room?{id, name, mode, node}}], id?}`, `rooms{rooms[{id, name, mode, players, max_players, open, props}], id?}`, `rpc_result{id, ok, status?, body, error?}`, `pong{ts, server_ms}`,
`notice{text, level}`, `error{code, message, id?}`, `desync{tick, actor, strikes, untrusted}`.

`id` (a number up to 2^32 − 1) tells apart the answers to requests in flight at once: the node
repeats the `id` of a `stats`, `find_friends` or `get_rooms` in its answer, and the `id` of any
of these or of an `rpc` in the `error` that refuses that request (`lookup_rate`, `rpc_rate`, a
bad `expr`, name or body). A request sent without an `id` is answered without one.

A node that lists `join_id` in `supports` takes an `id` in `create`, `join`, `join_or_create`,
`join_random` and `leave` too and repeats it in the `joined`, the `left` (reason `left`) or the
`error` that answers the request. The node answers these in the order it was asked; with the
`id` a client that gave a join up does not take the answer to it for the answer to its next
join. A node that does not list `join_id` refuses the `id` in these five as a field it does
not know.

`left.room` is the id from `joined.room` of the room the session is out of, whatever the reason
(`left`, `kicked` or the reason a kick gave, `room closed`, `session_replaced`, `rehost`). A
`left` can reach a client that has already joined another room; it then names the earlier one.
It is absent only in the answer to `leave` from a session that was in no room.

`joined.data` = `{host, port, cert_sha256, ticket(base64), ws_tunnel, webtransport_url?}`: connect QUIC
(ALPN `aurix-net/1`, pin `cert_sha256`), send the ticket as a datagram, wait for the
empty reliable frame. Datagrams can be lost, so repeat the ticket until the node's first
datagram arrives: the node ignores repeats, and drops every datagram that reaches it before
the ticket. A connection that shows no valid ticket within 5 s is closed with code 1. Where
UDP is blocked send the ticket as the first binary WebSocket frame and use binary frames as
datagrams.

A frame the room refuses is answered with `error` on the control channel, whatever carries
the data path. Frames that were on their way when the session left its room (`left`, a kick,
the room closed) are not answered and not acknowledged. The node gives up a link whose
reliable messages stay unacknowledged ([modes](modes.md)): it closes a QUIC or WebTransport
connection with code 4, and answers the next frame of a tunnel with one `error` `transport`
(`data path closed: link dead`) and drops the frames after it. A binary frame that is no
ticket, from a session in a room without a tunnel, is answered once with `unauthorized`
(`bad data ticket`).

The node holds up to 512 control messages for a client that is not reading its socket. One
more that the client must not miss (an answer, `left`, a message of the room, `lobby_update`,
a reliable event) ends the session: the WebSocket is closed with code 1008 and the reason
`control queue overflow`, and the player is out of the room as after any lost connection. A
client that does not read may see only the connection end.

Unreliable events for a player without a data path wait in a queue of their own, 512 of
them; one more is dropped and counted. They take no room from the messages above. The
node sends from the control messages, the unreliable events and the frames of the tunnel in
turn, so none of them holds up the others, and such an event may arrive after a control
message the node made later.

Every refused request of the player is answered: a lockstep command, an event over the rate.
Frames the node cannot read are answered `frame_errors_per_sec_per_player` times a second (10,
in bursts of up to twice that); the rest are only counted.

### Capabilities

What a side can do beyond the base protocol it lists by name in `supports` of its first
message: the node in `hello`, the client in `auth`. A side uses a capability only with a peer
that listed it and ignores the names it does not know; a message without `supports` lists
none. Every later addition to the protocol is announced this way.

A control message with a field the node does not know is refused with `invalid_request`
that names the field, in the message itself and in `target` of `event`; the node does not
pass over what it cannot act on. `props`, `player_props`, `filter`, `expected` and `body`
are the game's own and take any key. A client passes over a field it does not know in a
message of the node.

| Name | The side |
|---|---|
| `unsequenced` | takes channel 4 (`unreliable_unsequenced`), see Data path |
| `join_id` | repeats the `id` of a `create`, `join`, `join_or_create`, `join_random` and `leave` in its answer (the node) |

## Data path

12-byte header: `u8 version, u8 kind, u16 seq, u16 ack, u32 ack_bits, u16 tick`. `kind`
low bits = channel (0 unreliable-sequenced, 1 reliable-ordered, 2 reliable-unordered,
4 unreliable-unsequenced) or 3 for a fragment; `0x40` has-ack, `0x20` batch (payload =
`u16 len, bytes` repeated), `0x80` ack-only. Max datagram 1100 bytes, messages up to 64 KiB
fragmented. Frames inside: relay event frame or a mode frame (`docs/modes.md`).

The two unreliable channels differ in what happens to a datagram that arrives after a later
one. Channel 0 drops it: use it for state, where a newer message replaces an older one
(a position). Channel 4 delivers everything that arrives, in the order it arrives: use it for
events that are all wanted (a shot, a hit). A channel 4 datagram always has `0x20` set and
counts `seq` on its own; the receiver does not check it. A client sends on channel 4 only to a
node that listed `unsequenced`, and on channel 0 otherwise. The node passes an event on by the
channel it came on, and by channel 0 to a client that did not list `unsequenced`.

### WebTransport (browsers)

Nodes also listen for WebTransport sessions (`transport.webtransport_addr`, default
`0.0.0.0:7443`; `joined.data.webtransport_url` = `https://host:7443/`). The protocol is the
same as raw QUIC: the ticket datagram comes first, then packets flow as HTTP/3 datagrams
through the same link/channels. Browsers pin the certificate via `serverCertificateHashes`
(= `cert_sha256`), so the self-signed dev certificate works; it is valid 13 days and
regenerated on every start. The WebSocket tunnel remains the fallback. Manual browser check:
`sdk/web/test/browser.html` (two clients, echo over WebTransport) and
`sdk/web/test/browser-sync.html` (a sync room: avatars and a tagged spawn).

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
| `net_sync_inputs_rate_limited_total`, `net_sync_invalid_inputs_total`, `net_sync_late_inputs_total` | input frames over the rate (dropped), malformed or too far ahead (dropped), behind the room tick (late) |
| `net_lockstep_desyncs_total` | hash reports disagreeing with the majority |
| `net_rooms_adopted_total` | rooms taken over from a dead node |
| `net_control_overflows_total` | control sessions closed because the client did not read: the queue was full for a message it must not miss |
| `net_control_dropped_total` | unreliable events dropped on the control channel because the session held 512 of them unread |

## Usage metering

Every node adds per project and hour: `room_minutes`, `player_minutes`, `bytes_in`,
`bytes_out`, and `rooms_created` when a room is created (`GET /v1/usage?from&to`). Billing is
node-hours plus included traffic, never peak CCU. `GET /v1/regions` lists live regions with
`endpoints` (node HTTP URLs) so clients
pick the closest node; a room lookup on another node answers `conflict room_elsewhere:<url>`.
One call connects, joins and follows that redirect: `Client::connect_and_join` (Rust),
`Client.connectAndJoin` (Web), `ManagedClient.ConnectAndJoinAsync` (C#),
`aurix_net_connect_and_join` (C), `Client::connectAndJoin` (C++), `connect_and_join` (Godot).
A plain `join` returns the error.

## Webhooks

POST JSON `{event, project, at, data}` with `X-Aurix-Event` and
`X-Aurix-Signature: sha256=<hex HMAC-SHA256(secret, body)>`. Events: `room.created
{room, name, mode, region}`, `room.closed {room, name, mode, props, state?}`,
`player.joined {room, actor}`, `player.left {room, actor, kicked?}`,
`match.found {room, mode, players, tickets, bots}`,
`desync {room, tick, actor, strikes, untrusted}`,
`room.rehosted {room, name, mode, node, restored}` (room adopted by another node after a drain
or a node failure). Three attempts; 4xx stops retries. [webhooks.md](webhooks.md) has every
field.

Two events are synchronous calls rather than notifications: `join.validate` (the receiver
answers `{"allow": bool, "reason"?}` within 2 s; see Keys, grants, scopes) and `rpc` — a
client's `rpc{name, body}` is POSTed as `data: {name, player_id, actor?, room?, body}` to the
first webhook subscribed to `rpc`, which has 5 s to answer up to 16 KiB of JSON; the status and
body go back to that client as `rpc_result` (WebRPC: leaderboards, inventory, purchases
without exposing your backend's URL or auth to the client — the node signs every call). One
attempt, no retries.

## Matchmaking flow

1. Backend (or client with a `rooms` key through your backend) files a ticket. `player_id`
   is held to the rule of a grant: 1 to 128 characters, none of them a control character.
   A banned player gets no ticket: 403, as for a grant, counted under `banned` in
   `/v1/rejections`. `max_players` is at most the project's `max_players_per_room`,
   `props` are held to the size of a room's props (`max_props_bytes`), `region` is one of
   `GET /v1/regions` and `match_type` one of `fill`, `serial`, `random`; anything else is
   refused with 400.
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
redirect to the node that hosts one. A `match_type` other than `fill`, `serial` or `random`
is refused with `invalid_request`.

Bot backfill: `bots: true` on a ticket means "if `max_wait_secs` runs out short-handed, start anyway
and report the empty seats". When every member of a group agreed, the ticket state and the
`match.found` webhook carry `bots: <seats>`; the game (room master or WASM logic) spawns that many
AI players — the service never fakes players itself.

`join_or_create` takes the room fields of `create`, `visible` and `open` among them. They shape
the room only when the call creates it: a room that already exists stays as it is.

The room list, `get_rooms`, `join_random` and `fill_existing` offer a room only while it is
`visible`, `open` and has a free seat. A room with `visible: false` is still entered by its
name; a room with `open: false` takes no new player at all. The player whose call creates a
closed room is in it all the same, and a player who lost the connection returns to the seat
that is kept for him.

Party slots: `reserved: ["p2","p3"]` on room creation (REST, `create`, `join_or_create`) holds those seats
for 60 s — other players see the room as full until the party has arrived or the hold lapses: a held
seat is not a free one, so a room with no other seat left is offered to the party alone. Matchmaking
results carry the `players` list; pass it as `reserved` when the first of them creates the room.
More names than the room has seats (the creator's own seat counted), or more than 32, are
refused with `invalid_request`.

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
| HTTP requests per IP | 200/s (burst 400) | 429 |
| WebSocket connects per IP | 600/min | 429 |
| Concurrent sessions/data connections per IP | 512 | 429 / connection refused |
| Failed `auth` per IP | 20/min | `rate_limited`, connection closed |
| Room name | 1..=64 chars, no control characters | `invalid_request` |
| `lobby_props` per room | ≤32 keys, each 1..=64 chars | `invalid_request` |
| Control message (post-auth) | 68 KiB; 8 KiB before `auth` | connection closed |
| Write calls per API key (rooms, tickets, keys, webhooks, modules) | 20/s | 429 `rate_limited` |
| Grants per API key (`POST /v1/grants`) | 500/s | 429 `rate_limited` |
| Pre-auth control message | 8 KiB, 10 s to authenticate | connection closed |
| Request body | 64 KiB (module upload 8 MiB) | 413 |

`GET /healthz` answers 503 `{"ok":false,"draining":true}` while the node shuts down; players
in its rooms get `left{reason:"rehost"}` and rejoin the same room name on any node. Before
that the node stops taking packets on the data path, answers `event` on the control channel
with `error` `conflict` (`conflict: node_draining`) and waits, for at most half of
`server.shutdown_secs`, until the players have acknowledged what it sent them: a reliable
event the node acknowledged has reached the others in the room by the time `left` comes,
and one it did not acknowledge, or refused, was not delivered. Reliable delivery holds
within one connection; nothing is carried over to the room on its new node.
`GET /metrics` requires `Authorization: Bearer <server.metrics_token>` when that is set.

## Audit log

Every admin/operator mutation writes a row to the `audit` table (`actor` is `key:<api key id>` or
`operator`): `project.create`, `project.limits`, `project.suspend`, `project.resume`, `project.delete`, `node.drain`, `node.undrain`, `room.close`, `webhook.test`, `player.kick`, `player.ban`, `player.unban`, `webhook.rotate`, `operator_token.create`, `operator_token.revoke`, `room.notice`, `project.notice`, `grant.issue`, `key.create`, `key.revoke`, `webhook.create`,
`webhook.delete`, `module.put`, `module.delete`. Rows are kept for `retention.audit_days` (90).
