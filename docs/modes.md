# Room modes

One server, one client, one wire format. A room is created in one of three modes; the mode
decides who runs the game simulation and what travels on the data path.

| | `relay` | `sync` | `lockstep` |
|---|---|---|---|
| Simulation runs on | clients (your code) | server (authoritative) | every client (deterministic) |
| Client sends | events | inputs | inputs |
| Client receives | events | state deltas | everyone's inputs |
| Cheating resistance | none | strong | medium (hash checks) |
| Late join | cached events | snapshot | journal tail / checkpoint |
| Typical use | lobbies, turn-based, co-op, chat-like traffic | shooters, MMO-lite, anything server-authoritative | RTS, fighting, physics sims, anything that must replay bit-exactly |

All modes share: WebSocket control channel, QUIC/WebTransport data path (WebSocket tunnel
when UDP is blocked), four channels (unreliable-sequenced, unreliable-unsequenced,
reliable-ordered, reliable-unordered), rooms/lobby/properties/master client, grants and quotas.

The two unreliable channels differ in what a late datagram costs. On the sequenced one each link
(client to node, node to receiver) numbers its own datagrams and drops one that comes after a
newer one, so a message passed on by the node can be dropped on either link. That suits state,
where a newer message replaces an older one (a position), and is what `sendEvent` uses. On a path
that reorders packets it costs more than the packet loss: with 100 ± 30 ms of delay and no loss
about half of 60 relay events a second reached the other client. Events that are all wanted (a
shot, a hit) go on the unsequenced channel with `sendEventOn` (docs/sdk.md): it delivers every
datagram that arrives, in the order it arrives, and under the same delay all of them arrived.
Neither channel sends again, so a lost datagram is lost on both; reliable messages all arrive.

The two reliable channels differ the same way. The ordered one hands messages over in the order
they were sent, so one lost datagram holds back every message behind it until it is sent again.
The unordered one hands each message over as it arrives. Events that do not depend on each
other (a chat line, a pickup) go on the unordered channel (`"channel": "reliable_unordered"`):
under 10 % loss and 100 ± 30 ms of delay at 62 events a second a message waited 216–233 ms in
the middle case there against 399–464 ms on the ordered channel.

A link carries no congestion control of its own: QUIC applies its own to datagrams (RFC 9221),
and under the WebSocket tunnel TCP does.

A link holds at most 1 MiB of reliable messages that wait for the other side
(`RELIABLE_BACKLOG_BYTES`, the same in the SDKs): 87 s of 60 events a second of 200 bytes, or 16
messages of the largest size. A sender that outruns the receiver by more has lost it: the link
ends and `send` fails, as it does on a link that is already gone. A link also ends when a
reliable message has been resent and has gone the idle timeout of the transport under it
(`transport.idle_timeout_ms`, 15 s) without acknowledgement, whatever else arrives from the
other side. A message whose time ran out before it was resent once, because the timer of the
link came late, is resent and waits that long again. A player whose link ended stays in the
room on the control channel, where relay events still reach it.

## Topologies

A mode says who simulates; a topology says where that runs and how players reach it. Aurix
covers four of them on one protocol and one SDK surface:

| | Dedicated server | Client host | Shared authority | Deterministic |
|---|---|---|---|---|
| Mode | `relay` | `relay` | `sync` | `lockstep` |
| Who runs the game | your headless server (UE/Unity/C++) | one player's machine | the node (WASM module or schema rules) | every client, node referees |
| Where the room lives | a node **on the same machine** as your server | the node nearest the host | the node | the node |
| Authority | ++ server | −− host can cheat | ++ room, clients own assigned objects | ++ inputs on the node, hash checks |
| Path for players | direct to your machine (QUIC/WebTransport) | player → node → host | direct to the node | direct to the node |
| Host migration | restart the server; room row is adopted by the next node | ++ turnkey (`AllowHostMigration`) | n/a (node failure: adopt) | n/a (checkpoints) |
| Cost | your machine (+ node management on a BYOC plan) | players + a node | node-hours or shared plan | node-hours or shared plan |

**Dedicated server with a public IP.** Rooms live on the node a backend connects to: there is no
separate placement step, and a client that asks another node for the room by name is redirected
(`room_elsewhere`) - the connect-and-join call of every SDK follows that (docs/api.md). So the dedicated topology is one rule:
run a node next to the game server and let the server create its room through the local node
(`http://127.0.0.1:8090`); players use the project's usual URL and end up connected straight to
your machine. The node is part of your cell (`aurix-net-agent install` on that host, or the
self-hosted compose), so grants, matchmaking, replays and the console keep working. SDK helpers:
Unity `NetworkManager.StartDedicated(localNodeUrl)`, Unreal `DedicatedNodeUrl` in Project
Settings (used automatically by `-server` builds), Web/C++/Godot: connect with the node URL.

## relay

Create with `mode: "relay"` (default). Events are opaque bytes with a code, a target
(`all`, `others`, `master`, explicit actors), an optional interest group and a cache flag so
late joiners receive them. Rate and size limits come from the project quota.

Room props `player_ttl_secs` (default 30) and `empty_ttl_secs` (default 5) set how long a
disconnected player keeps their slot and how long an empty room stays open (both capped at
300). Interest groups are `u16` (65 535 groups). A player who connects again while still online
takes over their own slot; the old session gets `left` with reason `session_replaced` and is
ignored from then on.
`cleanup_cache_on_leave` (default true) drops a player's cached events when they leave for good; set it
to false to keep them for late joiners ("keep the cache on leave").
Cache slices: the `cache_slice{op, index?}` control message stamps events
cached from then on — `increase` opens the next slice, `set` picks one, `purge` drops the events of
one slice, `purge_up_to` every slice up to and including `index` (e.g. per-round state a late
joiner no longer needs). Late joiners receive the remaining slices in original order.

Room flags (room props): `publish_user_id` (default true; false hides other players' `player_id` from clients — actor numbers, names and props only; operators still see ids), `suppress_room_events` (default false; true drops `player_joined` / `player_left` broadcasts for very large rooms — master changes still go out), `suppress_player_info` (default false; true sends a joiner only their own roster entry), `broadcast_props_to_all` (default true; false skips the setter when a props change is broadcast). `CheckUserOnJoin` is always on: one session per player id per room is enforced by the grant.

Rate limits: a room takes `max_events_per_sec_per_room` events (default 4000). A player may send
`max_events_per_sec_per_player` events (default 120, burst of 2 s worth) to each receiver he
names (`actors`, `master`) and as many again to the room as a whole (`all`, `others`), so a host
that relays a game engine's packets to every client is held to the same rate per client as a
client is towards the host. Over the limit the event is dropped with `rate_limited`. Both are
project quotas (`/v1/operator/projects/:id/limits`).

### Lobby statistics and presence

`stats` answers with the project's live rooms and players and the same counts per lobby, where a room's
lobby is its room prop `lobby` (always part of the lobby listing; rooms without it form the default lobby
`""`) — lobby statistics without subscribing to the room list. `find_friends{players}` (≤ 64 ids)
answers with `online` (an authenticated control session or a room slot on any node of the fleet) and the
room/node each player is in — find friends. Both fan out across the fleet, so a session may
send at most one per second. Player ids are whatever your grants carry: a friends list itself stays in
your backend.

## sync

Create with `mode: "sync"` and optional props:

| prop | default | meaning |
|---|---|---|
| `tick_hz` | 60 | simulation rate (8..128) |
| `schema` | built-in arena | IDL text, see below |
| `budget_bytes` | 1100 | max delta payload per client per tick |
| `aoi_cell`, `aoi_radius` | 32, 2 | interest grid cell size (world units) and radius in cells; `aoi_cell: 0` disables AOI |
| `stale_after` | 30 | ticks after which an entity not sent to a client gets top priority regardless of distance (eventual consistency bound) |
| `position` | `Transform.0` | `Component.field` holding the vec3 used for AOI |

Clients send `Input` frames on the unreliable channel. A frame carries the inputs of the last 3
ticks, so each input rides in three frames; when no input follows, the client sends the last frame
again on each of the next two room ticks, the same three times as the inputs before it. The server
runs the simulation, replicates per-client deltas (change masks per component, baselines per
client, priority by change/distance/staleness) and keeps a hitbox history for lag
compensation. Simulation events reach clients on the reliable channel. Every 5 s the frame is
checkpointed to Redis for re-hosting.

Without a `schema` the room runs the built-in arena logic (one avatar per actor, input =
`i8 move_x, i8 move_z, u8 buttons`). With a `schema` and no `logic` the room is
owner-authoritative: each actor owns its avatar entity and its input is a list of full component
writes that the server applies and replicates. An input starting with `0xFF` is instead a list
of ops on entities the actor owns — `write` (any owned entity), `spawn` (a new owned entity,
at most 32 per actor, with initial writes), `despawn`, `set_policy` (per entity: `destroy_on_leave` default, `to_master`, `orphan`, `dynamic` — what happens when the owner leaves and who may take it), `request` (take an unowned or dynamic entity; the cell arbitrates), `release` (drop ownership, entity stays), `transfer` (hand ownership to another
actor in the room; the new owner shows up in the next delta) — so a client can own several
objects (projectiles, pickups, vehicles) and pass them around; a leaving actor's entities are
despawned. With `logic`, the WASM module decides what inputs mean (below).

A spawn the client wants to find again is sent as a tagged spawn (op 7: `u32 tag, u8 n,
writes`); the tag is a number the client picks, counting up and never 0. The room answers the
sender alone with `0x14` (`u32 tag, u8 status, u64 entity, u32 tick`, big-endian, 18 bytes, over
`ReliableOrdered`) in the tick the op ran: status 0 with the entity as the deltas name it and
the tick it was created on, 1 when the spawn was refused (32 owned entities, or no room for
another), 2 when the writes do not follow the schema. 1 and 2 create nothing and carry entity 0.
Deltas are unreliable, so the entity may show up before or after its answer. The room keeps the
last 64 tags of each actor while the actor is in the room: the same tag again creates nothing
and is answered with the same entity, so a client that got no answer sends the op again in a
new input. The tags move with the room to another node and stay when a second connection of the
player takes the slot over; they go when the actor leaves or drops off, together with the
entities despawned then. `joined.next_spawn_tag` is the first tag the player has not used: a
client counts from there, and a room whose `joined` has no such field takes no tagged spawns
(the built-in arena, a `logic` module, an older node). Every sync room also sends each player
`0x14` with tag 0 for its avatar on entering. An SDK that hands raw data frames to the
application hands these over as well.

Deltas go over `UnreliableSequenced`; the client acks every delta (`0x13`: last tick + 32-bit
history, `net_replication::AckTracker`). A delta the client never got has its entities forgotten
on the server, so the next delta carries them in full instead of a diff against state the client
does not have; lost despawns are repeated. Clients that never ack get the plain "baseline = last
sent" behaviour.

Input frames are limited per client to tick rate + 10 % (burst of 8 frames); inputs more than
64 ticks ahead of the room are dropped. A frame in which every input is for a tick the room has
already simulated is late: the client is stamping its inputs too close to the room's tick for
its latency. A late input still runs when it is newer than every input the room has taken from
that client, so an op in it (a spawn) is not lost. The room runs one input of a client per tick,
in the order sent: the late inputs of a frame take the current tick and the ticks after it, and
an input sent for a tick they hold runs after them. An input waits at most three ticks this
way; one that would wait longer is left to the repeats in the frames that follow. The tick is
what tells a new input from a repeat: a repeat of an input the room has taken does nothing, and
a client must not send a different input under a tick it has already used. Counters: `net_sync_inputs_rate_limited_total`,
`net_sync_invalid_inputs_total`, `net_sync_late_inputs_total`.

The room tells each client how its inputs arrive, with the same frame as lockstep (`0x25`:
`u32 tick, i8 min_margin, u8 late`, big-endian, over `UnreliableSequenced`). Every 30 ticks a
client that sent input since the last report gets one: `tick` is the room's tick, `min_margin`
the least number of ticks by which the newest input of a frame was ahead of the room when it
arrived (0 is just in time, negative is late, held to -127..127), `late` the frames that came
late (at most 255). Frames dropped for rate or as invalid are not in it, nor is a frame with no
input newer than the room has taken from the client (a repeat); a client that sends nothing
gets nothing. A client stamps its inputs further ahead when the margin is negative and
may come closer when it stays high; one that ignores the frame works as before.

### Schema IDL

```text
component Transform {
    pos: vec3 @range(-4096, 4096, 0.01)   // quantized, 3 x 20 bits
    rot: quat                             // smallest-three, 32 bits
    speed: f32                            // raw float
}
component Health {
    hp: u8
    team: i16
    name: string(24)
    flags: bits(5)
}
```

Component flags go on the header line: `component Inventory @owner_only { ... }` is sent only
to the entity's owner (hidden information stays on the server), `component Score @global { ... }`
makes any entity carrying it relevant to everyone regardless of AOI. AOI itself: up to 4
viewpoints per client (the avatar + extras set with the `set_view{viewpoints, interest}` control
message — camera, scope target; spectators get their first viewpoint from it), `interest` forces up
to 64 entities into view regardless of distance (team mates, objectives), and an entity already in
view stays until it is one cell further out than the radius (hysteresis against flicker).

Types: `bool`, `u8..u64`, `i8..i64`, `bits(n)`, `f32`, `f32 @range(min,max,res)`,
`vec2`/`vec3 @range(...)`, `quat`, `string(max)`, `bytes(max)`. The blake3 hash of the
canonical schema is returned in `joined.schema_hash`; a client that sends a different
`schema_hash` on join is refused with `schema_mismatch`.

The input of one tick is at most 255 bytes: a raw input, the writes of a tick taken together,
one batch of ops, and a lockstep input. A longer one is refused by the client that is asked to
send it (`RangeError` in Web, `ArgumentException` in C#, `-1` with
`AURIX_NET_ERR_INVALID_ARGUMENT` in the C ABI and the clients built on it).

### Server-side game logic (WASM)

Set `props.logic` to the name of a module uploaded with `PUT /v1/modules/:name` and the
node runs it as the room's authoritative logic: `aurix_join` spawns the avatar,
`aurix_step` reads this tick's inputs and writes components, `raise` sends game events,
`hitbox` feeds lag compensation. In a lockstep room a module that exports
`aurix_command(actor, len)` gets every command of the tick before `aurix_step`, in the order
the clients get them, and reads its bytes with `command_read(ptr, cap)`; a module without
the export skips commands. The commands' sender `65535` is not a player: the module gets no
`aurix_join` and no input for it. Any language that targets wasm32 works; the ABI is plain
integers and floats (see `net-game`). Fuel and memory are capped per call/room, so one
broken game cannot stall a shared node. Without `logic`, a custom schema runs
owner-authoritative (clients write their own entities' components) and no schema at all runs
the built-in arena.

## lockstep

Create with `mode: "lockstep"` and `tick_hz`. The server is the clock: every tick it bundles
the inputs it received for that tick and sends the bundle (plus the two previous ones) to
every actor. Clients simulate with `net-rollback`: predict missing inputs with the actor's
last one, rewind and resimulate when a late input differs, hash the state every N ticks and
report it (`Hash` frame). The server compares hashes per tick; a minority is reported to
everyone as `desync`. The server keeps the canonical input journal, which is also the replay
format (`net-rollback::Replay`): seed + schema hash + inputs per tick reproduces the game.
`schema_hash` there is of the schema the node steps in a room with a `logic` module (below);
the node knows no schema of a room without one, and the field is empty.

An input frame carries the input for one tick and repeats the few before it, so a lost frame
costs nothing while the next one is still in time. An input that arrives after its tick was
bundled is late. The newest input of a late frame goes into the next bundle, so the player
keeps acting; the older ones in it are repeats whose tick has passed and are dropped. Every 30
ticks the room tells each client how early its inputs arrived (`InputStats`), and the client
tunes its input delay from that.

A client needs every bundle, in order. It tells the room which tick it needs next with an `Ack`
frame (`0x29`: `u32 next`, big-endian, over `UnreliableSequenced`; every tick before `next` has
arrived) after each bundle datagram it receives, and the room sends it every bundle from that
tick on in one datagram, never fewer than the last three. When they no longer fit a datagram
(1000 bytes, 255 bundles) the room sends the last three and, once, the missing bundles on the
reliable channel in the same layout, a part at a time as on join; when its journal no longer
holds them, a catch-up as on join. A client that sends no `Ack` gets the last three bundles each
tick and stops for good once three datagrams in a row are lost. Spectators are served the same
way up to their delayed tick.

**Commands**: a reliable message outside the per-tick input stream — buy, surrender, pause. The server stamps it onto the next tick and ships it inside that tick's bundle as the input of pseudo-actor 65535, so it is in the journal, in catch-ups, in replays and in the referee's input stream; every client sees it at the same tick (`SendCommand` / `OnCommand` in the SDKs). A command takes 1 to 251 bytes; the node answers a longer one with `error` `invalid_request` on the control channel. A tick carries 255 bytes of commands, 4 of them for the header of each: commands that do not fit ride the next ticks in the order they arrived, the same for clients, the journal, the replay and the referee. Up to 4096 bytes wait this way; a command past that is refused with `rate_limited`.

**Instant replay for sync rooms**: room props `replay_every` (ticks between snapshots, default hz/2) and `replay_secs` (window, default 30; 0 off) keep full-world delta frames on the node; `GET /v1/rooms/{id}/replay?from=&to=` returns them (`mode: "sync"`) and any SDK sync client replays them by feeding the frames, in order, to a fresh world. `from_tick` in the answer is the tick of the first frame returned, or `from` when there is none.

Use `net-fpmath` (Q32.32 fixed point, table trig, deterministic RNG) for anything that
touches game state; never floats.

### Server-verified lockstep

Give a lockstep room the same `logic` module (and `schema`) a sync room would use and the node
steps it too, over the canonical input journal. Then the node's state hash is the truth: a
client whose hash differs is desynced even if every client agrees with it, checkpoints for
late join and failover come from the node (no master upload), and after a node failover the
new node resimulates the journal tail from the last checkpoint. Clients that want to be
compared must follow the same rule: an actor's avatar spawns at the first tick it appears in a
bundle, state is a `net-sim` frame, hash = `hash_bytes(frame.snapshot())` after every tick that
is a multiple of 10 (tick 0 included), reported under that tick: the state after tick T is
numbered T. A checkpoint is numbered differently, by the tick its state starts from: `Catchup`
and `Restore` with tick T carry the state after tick T - 1, and the bundles from T follow.
On a desync the node stores its own state at that tick as the reference dump (actor `0` in
`GET /v1/rooms/:id/desyncs`) next to the desynced client's, so nobody has to trust the master;
room details report `referee: true` while the node hosts such a room.

A desynced client, in a room with a referee or without, is sent `Restore` (`0x2A`) on the
reliable-ordered channel: the newest checkpoint in the layout of `Catchup` (u32 tick, u64 hash,
u32 length, state), followed by every journal bundle from that tick. The client goes back to
that tick whatever it has simulated since, which is why the frame is not `Catchup`: a client
drops a `Catchup` behind the ticks it already has. `Restore` comes with each of the first three
strikes; the third reports the client `untrusted`, and from the fourth it is no longer restored,
so a client lying about its hash cannot make the node re-send the state every few ticks.

`crates/net-sim-guest` is the reference module for this: `physics-arena`, avatars as fixed-point
circles in a walled `net-sim-physics` world (input = i8 move_x, i8 move_z), built with
`cargo build --release --target wasm32-unknown-unknown -p net-sim-guest` and uploaded via
`aurix-net module-put physics-arena target/wasm32-unknown-unknown/release/net_sim_guest.wasm`.
Clients running the same `net-sim-wasm` physics hash identically to the node.

### Late join and rejoin

A lockstep room never stops for a joiner and never migrates a host. Every
`CHECKPOINT_EVERY_SECS` (5 s) the server asks the master for its newest *confirmed* state
(`CheckpointReq` on the reliable channel; the master answers with `Checkpoint`: tick, hash,
<= 60 KiB of state — use `Rollback::confirmed_state()`). When a data link attaches to a room
that is past tick 0 (late join, or a reconnect into an offline slot) the server sends, on the
reliable-ordered channel, `Catchup` (the newest checkpoint, or tick 0 with an empty state
meaning "start from the seed") followed by every journal bundle from that tick up to the
current one, in ~32 KiB frames. The bundles leave as fast as the joiner takes them: each tick
the room sends as many frames as keep the joiner's link under half of its 1 MiB, and the ticks
made in the meantime follow in the same order, so a long catch-up neither ends the link nor
piles up on the node. Live bundles keep flowing unreliably in parallel, so the
client buffers them until the reliable tail reaches a tick the live stream already covers
(the tail always overlaps thanks to bundle redundancy). `joined.tick` and `joined.seed` tell
the client where it enters; the seed is a random number below 2^53, so it survives a JSON
reader that keeps numbers as doubles. Start the client with `Rollback::from_checkpoint(cfg, actors,
tick, state)` and feed the tail bundles as confirmed inputs. If a joiner had to replay from a
stale checkpoint the server asks the master for a fresh one at once, so catch-up stays bounded.

The room keeps 4 hours of journal at 60 Hz (`MAX_JOURNAL_TICKS`, 864 000 ticks), from the
newest checkpoint when it has one. A join or a return after a disconnect that the journal can
no longer serve (no checkpoint and the journal no longer starts at tick 0, or a checkpoint
older than the journal) is refused with `conflict` and a message that starts with
`no_checkpoint`. The link of a joiner too slow for the catch-up ends once the journal has moved
past the tick it waits for. A replay of a match longer than the journal does not start at
tick 0. The journal takes 72 bytes a tick for two players with 8-byte inputs and 216 bytes
for eight with 16-byte inputs: 16 MB and 47 MB an hour, 62 MB and 187 MB at 4 hours. A game
that sends checkpoints keeps the catch-up of a joiner to the last seconds instead.

## Spectators

`join{name, spectator: true}` watches a room without taking a slot: spectators are not counted
against `max_players`, never become master and cannot send events or inputs (`forbidden`).
Relay: they receive events like everyone else. Sync: they get a view of the whole world (no
AOI, no avatar). Lockstep: they get the input stream `spectator_delay_secs` (room prop, default
3) late, and catch-up only up to that delayed tick, so a streamer's viewers cannot feed live
positions back. `player_joined.player.spectator` tells clients who is watching.

## Node failure: rooms move, they do not die

Every 5 s a node writes each sync/lockstep room to Redis (`net:<project>:ckpt:<room>`, TTL
10 min): the player list (actor ids, player ids, names, master), the simulation snapshot for
sync, and for lockstep the newest master checkpoint plus the input journal from that tick.
When a node misses three heartbeats the others mark its rooms `orphaned` instead of closing
them. The first node a player of that room reaches (plain `join` / `join_or_create` by name
on any node of the region) atomically adopts the row, rebuilds the room from the DB config,
restores the simulation from the checkpoint, recreates every player as an offline slot (so
reconnects get the same actor ids, `rejoin_actor` works) and restarts the tick loop. Lockstep
joiners then get the normal catch-up; sync clients get fresh baselines. Lockstep rooms also
append every tick's input bundle to a Redis list (`net:<project>:journal:<room>`, flushed every
6 ticks), and the adopting node replays the entries after the checkpoint, so at most ~100 ms of
inputs are lost; sync rooms checkpoint every 2 s. The list holds as many ticks as the journal of
the room, and the hashes of the room are kept beside it (`net:<project>:hashes:<room>`), so
the adopting node has the ticks from before the checkpoint too and the replay of a room that
moved starts where it did before. For two players with 8-byte inputs the two lists take 31
bytes a tick: 6.6 MB of Redis memory for an hour of a match at 60 Hz, 26 MB at the 4 hours the
journal keeps. Adopting a room reads them once: 120 ms for that hour. The lists and the
checkpoint are deleted when the room closes. A `room.rehosted` webhook fires; orphans nobody adopts within
10 min are closed. Relay rooms are re-created with name/props/slots but no event cache.

## Data frames (first byte)

| kind | direction | mode |
|---|---|---|
| event frame (no kind byte, see `net-rooms::EventFrame`) | both | relay, sync events |
| `0x10` Input | client -> server | sync, lockstep |
| `0x11` Delta | server -> client | sync |
| `0x12` Resync | client -> server | sync |
| `0x13` Ack (last delta tick + 32-bit history) | client -> server | sync |
| `0x14` Spawned (tag, status, entity, tick; tag 0 is the avatar) | server -> the client that spawned (reliable) | sync |
| `0x20` InputBundle | server -> client | lockstep |
| `0x21` Hash | client -> server | lockstep |
| `0x22` Checkpoint | master -> server (reliable) | lockstep |
| `0x23` CheckpointReq | server -> master (reliable) | lockstep |
| `0x24` Catchup | server -> joiner (reliable), then `0x20` bundles | lockstep |
| `0x25` InputStats (tick, least input margin, late inputs; every 30 ticks) | server -> client | sync, lockstep |
| `0x29` Ack (the tick the client needs next) | client -> server | lockstep |
| `0x2A` Restore (layout of `0x24`) | server -> desynced client (reliable), then `0x20` bundles | lockstep |
| `0x28` Command | client -> server (reliable); delivered inside the next `0x20` bundle as the input of pseudo-actor 65535 (`[u16 actor, u16 len, bytes]*`) | lockstep |
