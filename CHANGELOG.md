# Changelog

All notable changes to Aurix Net. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow SemVer. Nodes of one deployment run the same minor version; the `aurix-net:<tag>`
image, the `sbom` artifact and this file's entry are produced from the tagged commit
([release checklist](docs/release-checklist.md)).

## [Unreleased]

### Added
- Data path: channel 4, unreliable and unsequenced. It delivers every datagram that arrives,
  in the order it arrives, where channel 0 drops one that comes after a later one: under a
  delay of 100±30 ms with no loss about half of the unreliable relay events were dropped
  that way. An event picks it with `"channel": "unreliable_unsequenced"`, the Rust client
  with `send_event_on`; channel 0 stays the default and the one for state. The node and the
  client announce it in the new `supports` list of `hello` and `auth`, and fall back to
  channel 0 with a peer that does not.
- SDKs: `sendEventOn` sends a relay event on a channel of the caller's choice, which is how one
  goes on channel 4: Web `sendEventOn`, Unity `SendEventOn`, C `aurix_net_send_event_on`, C++
  `sendEventOn`, Godot `send_event_on`, each with the new value of its channel list
  (`Channel.UnreliableUnsequenced`, `AURIX_NET_CH_UNRELIABLE_UNSEQUENCED`,
  `CHANNEL_UNRELIABLE_UNSEQUENCED`). The Web and managed C# clients list `unsequenced` in `auth`
  and send on channel 0 to a node that does not list it. In Unreal `SendEventEx` takes
  `EAurixNetChannel::UnreliableUnsequenced`.
- SDKs: `joinOrCreate` takes `visible` and `open`, which the node applies to the room the call
  creates: Web `joinOrCreate`, Unity `JoinOrCreateAsync` and `JoinOrCreate` (and
  `JoinOrCreateAsync` of `NetRunner`), C++ `joinOrCreate`, Godot `join_or_create`. Only the
  Unreal plugin sent them before; from the other SDKs a hidden or closed room could be made by
  a raw message alone.
- C ABI: `aurix_net_set_last_error` makes an error the last one of the calling thread. The
  error is kept per thread, and a wrapper that runs a call on a worker thread reports its
  failure on another.
- Unreal sessions: a `Near` search param on a number orders the results of Find Sessions by how
  near the room's value is, as it does in the Steam and EOS subsystems of the engine. It failed
  the search before.
- Control channel: `stats`, `find_friends` and `get_rooms` take an optional `id`, which the node
  repeats in the answer (`stats`, `friends`, `rooms`) and in the `error` that refuses that
  request; an `error` that refuses an `rpc` carries its `id` too. Two lookups in flight at once
  could not be told apart before.
- Sync: a spawn can carry a tag of the client's (op 7), and the room answers the client with
  the entity it created (`0x14`), or with the refusal. A client could not tell the entity it had
  spawned from its avatar or from another spawn, and a spawn sent twice was created twice. The
  room remembers the last 64 tags of a player, also across a move to another node, so the op can
  be repeated until answered; `joined.next_spawn_tag` says where a client's tags start, and
  every sync room now names the avatar the same way (`0x14`, tag 0). `spawn` without a tag works
  as before ([modes](docs/modes.md)).
- SDKs: `spawn` in a `sync` room sends the tagged spawn and returns the tag; the room's answer
  reaches the game as an event (created with the entity, refused, invalid). The SDK counts the
  tags and repeats a spawn until it is answered. The client keeps the avatar the room announces
  from the join on. C `aurix_net_sync_spawn` / `aurix_net_sync_poll_spawn` / `aurix_net_avatar`,
  C++ `Sync::spawn` / `pollSpawn` and `Client::avatar`, Godot `spawn_entity` / `spawn_answer`
  and `AurixNetClient.get_avatar`, Web `SyncSession.spawn` / `onSpawnAnswer` and
  `Client.avatar`, Unity `NetRunner.SpawnEntity` / `OnSpawnAnswer` / `Avatar`. C++, Godot and
  Web `spawn` returned the input tick before; on a room that does not answer tagged spawns it
  now fails ([SDKs](docs/sdk.md)).
- Rooms: `left` names the room it is about (`room`, the id from `joined.room`) for every reason:
  own leave, kick, closed room, `session_replaced`, `rehost`. A `left` that arrived after the
  client had joined another room could not be told from one about the new room. Clients that
  ignore the field work as before.
- Sync: the room reports to each client how early or late its inputs arrive, with the frame
  lockstep rooms already send (`0x25`: tick, least margin in ticks, late frames; every 30 ticks
  to a client that sent input). A client had only its ping to guess the lead from and was told
  nothing when its inputs came late. Clients that ignore the frame work as before.
- C ABI: `aurix_net_last_error_code()` says why the last failed call failed as one of the
  `AURIX_NET_ERR_*` constants of `aurix_net.h` (bad argument, HTTP, connection, data path,
  disconnected, timeout, protocol, and the node's refusals: unauthorized, not found, room full,
  room closed, schema mismatch, rate limited and the rest). Until now the only way to tell a full
  room from a missing one was to search the error text. The C++ wrapper carries the value in
  `aurix::net::Error::code`, the Godot nodes return it from `get_last_error_code()`
  (`AurixNetClient.ERROR_*`).
- Metrics: `net_sync_late_inputs_total` counts the input frames that reach a `sync` room with
  every input in them for a tick it has already simulated. They left no trace.
- Rust client: `Joined` has the lockstep room's `seed`. The node sent it in `joined`, the client
  dropped it, so a game on the Rust client could not start from the state the node starts from.
- C ABI, C++, Godot, C#: the lockstep room's `seed` is there too: `aurix_net_joined_seed()`,
  `aurix::net::Client::seed()`, `AurixNetClient.get_seed()` and `ManagedClient.Seed`.
- SDKs: a connected client tells the game the player its grant names, the id and the name the
  node answered the sign-in with: `playerId` / `playerName` in Web, `PlayerId` / `PlayerName` in
  both C# clients, `aurix_net_player_id()` / `aurix_net_player_name()` in the C ABI,
  `playerId()` / `playerName()` in C++, `get_player_id()` / `get_player_name()` in Godot,
  `GetPlayerId` / `GetPlayerName` in the Unreal plugin, `player_id()` / `player_name()` in the
  Rust client. A game had the id only from the list of players of a room it had joined.
- C++ and Godot SDKs: `joinOrCreate` / `join_or_create` take `reserved`, the player ids whose
  seats the new room holds (the `players` of a matchmaking ticket), and `joinRandom` /
  `join_random` joins a random open room, as the Web and Unity SDKs do. Without `reserved` a
  matched player could not keep the seat of the other one, and a connected Godot client had no
  random join.
- Unity SDK: the client runs in a Web build. There it talks through the browser's WebSocket
  and timers (a `.jslib` plug-in) and makes its HTTP requests with `UnityWebRequest`; the link,
  its resends, `ping` and `link dead` are the ones of every other platform. A Web build
  compiled before and could not connect: Unity has no `System.Net` sockets, no `HttpClient`
  and no threads in a browser.

### Changed
- Rooms: `join_random` with a `match_type` other than `fill`, `serial` or `random` is
  refused with `invalid_request`. It was read as `fill`.
- Rooms: `reserved` with more than 32 names is refused with `invalid_request`. The node
  kept the first 32 and dropped the rest without a word.
- Matchmaking: a ticket with a `region` the deployment does not have, a `match_type` other
  than `fill`, `serial` or `random`, or `props` larger than a room's props may be is refused
  with 400. The first was queued under a region that does not exist, the second was read
  as `fill`, the third was taken at any size.
- Matchmaking: `DELETE /v1/matchmaking/tickets/:id` answers 204 for a ticket already
  cancelled and 409 `conflict` for a matched one. Both got 404 `not found`, though `GET`
  showed the ticket. 404 is left for a ticket the node does not have.
- Rooms: `reserved` with more names than the room has seats is refused with
  `invalid_request`, in `POST /v1/rooms`, `create` and `join_or_create`. Such a room was
  made and offered to nobody. A change of behaviour for a caller that sent it.
- REST: `POST /v1/bans` with `duration_secs: 0` is refused with 400. It used to ban for
  good, as a request without `duration_secs` does. A change of behaviour: a backend that
  sends 0 for a permanent ban leaves the field out instead.
- Lockstep rollback and instant replay (Rust, Web, C#, Unreal), breaking: the step of the
  simulation takes the commands of the tick as a third argument, `step(tick, inputs, commands)`,
  and a game that predicts takes its commands there. A command reached such a game only through
  `onCommand`, outside the step, so the next rollback and the local replay lost it and the
  client got `desync` in a room with a referee; in Rust the commands stayed among the inputs
  under actor 65535 and were repeated in every predicted tick as that actor's last input. A
  predicted tick has no commands, and a confirmed tick that carries some is stepped again with
  them. What to change in a game ([SDK](docs/sdk.md)):
  - Rust: `Simulation::step(tick, inputs, commands)`; `Rollback::confirm` and `ReplayRing::record`
    take the inputs of `LsEvent::Tick` as before and take the commands out themselves,
    `ReplayWindow` has `commands`, and `lockstep::take_commands` does the same for a game
    without prediction;
  - Web: `Simulation.step(tick, inputs, commands)`, `onTick(tick, inputs, commands)`,
    `LockstepRollback.confirm(tick, inputs, commands)`, `ReplayRing.record(tick, inputs, commands,
    stateBefore)`, `ReplayWindow.commands`;
  - C#: `ISimulation.Step(tick, inputs, commands)`, `LockstepRollback.Confirm(tick, inputs,
    commands)`, `ReplayRing.Record(tick, inputs, commands, stateBefore)`, `ReplayWindow.Commands`;
    inside `OnTick` the commands of the tick are `LockstepClient.Commands`;
  - Unreal: `OnSimulate` of `AurixRollbackRunner` is `(Tick, Inputs, Commands, bPredicted)`, and
    `ExportReplay` / `PlayReplay` keep the commands of a tick as its entry of actor 65535.

  `onCommand` / `OnCommand` stay as they were for a game without prediction.
- Unity SDK: `LockstepClient.SendInput` returns the tick the input is stamped with, or -1 when
  it sent nothing, as `sendInput` of the Web SDK does. It returned nothing, and a game that
  predicts had no tick to give `LockstepRollback.LocalInput`.
- Unity SDK: the package names Unity 2022.3 as the oldest editor it works in; it named 2021.3,
  in which it was never built. It builds, weaves and makes a player in 2022.3.62f3.
- Protocol: a control message with a field the node does not know is refused with
  `invalid_request` that names the field. The node used to pass such a field over, so a
  misspelt or unsupported option was lost without a word. A change of behaviour: a client
  that sends a field outside the documented ones gets a refusal where it got an answer.
- SDKs: the C library, and with it C++, Godot, Unreal and the native C# client, refuses a
  control message with a field the node does not know before it is sent, with the node's code:
  `AURIX_NET_ERR_INVALID_REQUEST`, and the text names the field. It reported
  `AURIX_NET_ERR_INVALID_ARGUMENT` for a message it could not read.
- Lockstep: the journal keeps the inputs of a tick packed in one buffer: 72 bytes a tick
  instead of 408 for two players with 8-byte inputs, 216 instead of 600 for eight with 16-byte
  ones.
- Dates: the node reads and writes dates as strict RFC 3339 through the `time` crate; its own
  parser and formatter are gone. What clients see:
  - webhook `at`, the usage and audit CSV exports and ban messages end in `Z`, as JSON already
    did, and no longer in `+00:00`;
  - the fraction of a second has as many digits as it needs (`.12`, not `.120`);
  - request dates must be RFC 3339. `UTC` for the offset, an offset without the colon (`+0200`)
    and unpadded numbers (`2026-1-5`) are refused.
- Limits: a rate limit of `0` (`http_rps_per_ip`, `ws_connects_per_min_per_ip`,
  `auth_failures_per_min_per_ip`, `writes_per_sec_per_key`, `grants_per_sec_per_key`) is refused
  at start. It used to start a node that refused every request of that kind.
- Event limits: `max_events_per_sec_per_player` is counted for each receiver a player names
  (`actors`, `master`) and once more for events to the whole room (`all`, `others`); it used to
  be one count for everything a player sent. The defaults are 120 per player (was 60) and 4000
  per room (was 2000). A project quota or a node `[limits]` value set by hand keeps its number
  under the new meaning.
- Web SDK: `Client.on` is typed per event (`ClientEvents`), so a listener gets the right argument
  types instead of `any`; control messages are `ControlMessage`, and `StateMachine.fromJSON` takes
  `StateMachineJson`. The sources follow Prettier and typescript-eslint (`npm run lint`,
  `npm run format:check`).
- C++ and Godot SDK: notices and the rehost signal read control messages with the library's JSON
  parser (new C ABI call `aurix_net_json_string`) instead of a hand-written string search.
- Unreal plugin, identity interface: the unique net id of local user 0 is the player's id on the
  cell, from the moment the plugin is connected until it is not, and the nickname is the grant's
  name. It was a random GUID made at startup, which no seat could be held for. Before the
  connection there is no id; `Login` and `AutoLogin` succeed once connected, `Login` in the
  editor connects with a development grant, and `Logout` disconnects. The id is written to the
  local player and its player state when the connection is made and taken away when it is lost,
  as the engine's own login node does: the Create Session node reads it there.
- Unreal plugin, session interface: `CreateSession`, `JoinSession` and `StartMatchmaking` no longer
  block the game thread for the round trip to the cell. They return at once and their completion
  delegates fire when the room is joined; until then the session state is `Creating`. Search
  results no longer carry the room name as `MAPNAME`.

### Fixed
- Godot SDK: a Windows build of the extension was not found by Godot. `SConstruct` names the
  library `libaurixnet_godot.windows.*.dll`, the way godot-cpp names its own test extension, and
  `aurix_net.gdextension` listed it without the `lib`, so the file had to be renamed by hand. The
  `.gdextension` file now lists the name the build gives; a library renamed to
  `aurixnet_godot.windows.*.dll` gets its `lib` back. On macOS `SConstruct` now builds the
  `.framework` the file lists. The file no longer names `aurix_net.dll` as a dependency on
  Windows: the Windows library has `libaurix_net` linked in and loads without it. The README has
  the Windows build from Linux and the MinGW it needs (POSIX threads), and CI builds the
  extension for Windows.
- SDKs: the native client (C, C++, Godot, Unreal, the native C# client, the Rust `net-client`)
  could not connect to a node behind TLS: `https://` failed with `websocket: URL error: TLS
  support not compiled in`, so it worked only with a node reached over plain HTTP. The control
  channel now goes over `wss://` with the certificate authorities the HTTP calls trust.
  `Options::roots` of `net-client` adds the authority of a node behind a private CA.
- Data path: a link whose timer ran late was given up without one repeat. A node or a Rust
  client that got no processor time for the idle timeout (15 s) ended the link the moment
  its timer ran, though the acknowledgement was waiting to be read. A link ends when a
  reliable message has been resent and has gone the idle timeout without acknowledgement;
  one whose time ran out before any resend is resent and waits again, as in the Web and C#
  clients.
- Control channel: a message for a client that had 512 unread ones was dropped without a
  word, be it `joined`, `error`, `left`, a message of the room or a reliable event, and the
  client went on with a state the node did not have. Such a session is closed with code
  1008 and the reason `control queue overflow`, counted in `net_control_overflows_total`
  and logged. An unreliable event is still dropped, and counted in
  `net_control_dropped_total`.
- Control channel: unreliable events for a player without a data path filled the queue of
  the messages he must not miss, and the first of those, a `player_joined` for one, closed
  his session. Such events wait in a queue of their own, 512 of them. Control messages,
  such events and the frames of the tunnel are sent in turn: with control messages always
  first, the tunnel of a client that read slower than they came stood still for seconds.
- Grants: the length of a `player_id` was counted in bytes when a grant was signed and in
  characters everywhere else, so a player named in 65 to 128 Cyrillic letters could be
  banned and queued for a match and could get no grant. It is counted in characters, by
  one function: 1 to 128, none of them a control character.
- Matchmaking: a ticket was taken with a `max_players` above the project's
  `max_players_per_room`, a room size the project cannot have. It is refused with 400.
- Matchmaking: a ticket was taken for a banned player and waited for a match he could not
  join. It is refused with 403, as a grant for him is, and counted under `banned`.
- Matchmaking: a ticket was taken with any `player_id`: empty, of any length, with control
  characters. It waited in the queue for a player no grant can be issued for. The ticket
  is refused with 400, by the rule a grant is held to.
- SDKs, sync rooms: the render tick, the one remote entities are drawn at, could pass the
  room's tick and stay ahead of it for the rest of the stay. It only ever moved forward: with
  frame time, and up to `interpolationTicks` behind each delta. Deltas three frames late put it
  two ticks ahead of the room, and after a node that stood still for 100 ms it stayed ahead for
  good, so every remote entity was drawn at its newest sample, with nothing to interpolate, in
  steps. The render tick now stops at the tick of the last delta, and the delta after that puts
  it back `interpolationTicks` behind; a delta that finds it short of that tick leaves it as it
  runs. The same in the Web `SyncSession`, C++ `Sync`, Godot `AurixNetSync` and the Unity
  `NetRunner` ([SDKs](docs/sdk.md#sync-rooms-from-c-and-godot)).
- Rust client and what is built on it (C, C++, Godot, the native Unity client, Unreal): a join
  right after `leave` over QUIC at times failed with `disconnected`, and every later join of
  that client failed with it: 3 runs of 9 that joined and left 150 times broke that way. The
  client closed the connection of the data path while the task that reads it was still
  running, and the task reported the end of the path as a lost connection. The tasks of a data
  path are now told to stop and have ended before its connection is closed, and a path the
  client closes reports nothing.
- Rust client: a join the client gave up, with no answer in time or a data path that did not
  open, left the player in the room on the node, and its late `joined` was taken for the answer
  to the next join. The client now leaves after such a join and passes over what the node
  answers before it answers that leave.
- Joins and leaves: a client told the node's answers apart by their order alone, and where the
  node's answer to a `leave` was lost, the Rust client passed over the answer to every join
  after it until it reconnected. `create`, `join`, `join_or_create`, `join_random` and `leave`
  now take an `id` that the node repeats in its `joined`, `left` or `error`, announced as
  `join_id` in `supports` of `hello`. The Rust client numbers its joins and leaves for a node
  that lists it and takes only the answer with its number; with a node that does not, it works
  as before. Its numbers go from 2^31 up (`OWN_IDS`), so a game numbers its `stats`,
  `find_friends` and `rpc` below that.
- Web and managed C# clients: the late `joined` of a join the client gave up was taken for the
  answer to the next join, and the player stayed in that room on the node. Both now send the
  `id` of a join and of a leave to a node that lists `join_id` and take only the answer that
  repeats it, and leave after a join that got no answer in time.
- Unreal plugin: after an async call failed, `JoinSession` into a closed room among them,
  `Get Last Error` was empty and `Get Last Error Code` was `None`: the call fails on a worker
  thread and the error is kept per thread. Both now tell the reason on the game thread, and
  `JoinSession` writes it to the log.
- Data path: a player who stopped acknowledging over the WebSocket tunnel kept its link for
  good, and the node queued every reliable message for it: 100 events a second of 60 000 bytes
  grew the node by about 1 MB a second. Over QUIC and WebTransport a link was given up after 20
  repeats, 1.2 to 30 s depending on the round trip. A link now ends the same way on all three:
  when a reliable message has stayed unacknowledged for the idle timeout of the transport
  (`transport.idle_timeout_ms`, 15 s), whatever else arrives from the other side. Its queue is
  freed and events reach the player on the control channel.
- Data path: `Link::send` took messages on a dead link, and the reliable queue of a link had no
  limit: 200 000 sends on a dead link were all accepted and kept. A dead link now refuses
  `send` (`LinkError::Dead`) and holds nothing. A link holds at most 1 MiB of reliable messages
  that wait for the window (`RELIABLE_BACKLOG_BYTES`); a sender that outruns its peer by more
  loses the link with `LinkError::Backlog`. `Link::new` takes the idle timeout of its carrier.
  `LinkStats::retransmits` counted first sends that had waited for the window as repeats.
- Lockstep: the referee took the commands' sender `65535` for a player, called `aurix_join`
  for it and never ran the commands, so a client that runs them got `desync` after the
  first one. A module gets the commands through the optional `aurix_command` export.
- Rooms: a room whose last free seat was held for a party member stayed in the room list
  and in `join_random` and `fill_existing`, so a stranger was sent there to get `room_full`.
  A held seat counts as taken for everybody but the player it is held for, until he arrives
  or the hold lapses.
- Hand-over: a node that stopped or was drained told the players to leave at once: reliable
  events it had taken from one player and not yet got through to the others were lost, and
  the sender was left without the acknowledgement of some it had taken. The node stops
  taking packets, waits for the players' acknowledgements for at most half of
  `server.shutdown_secs` and tells them to leave after that.
- REST: `POST /v1/grants` with a `ttl_secs` of 2^63 or more answered 200 with a grant that
  had expired before it was issued (`expires_at` in the past, negative for 2^63). Such a
  value is cut to the node's limit like any other, and `ttl_secs: 0` is refused with 400.
- REST: `GET /v1/me` answered a key used from an address outside its `allowed_cidrs` with
  the project (id, name, plan, limits) and the key's scopes, where every other call refused
  it. It refuses such a key with 403.
- REST: `GET /v1/nodes` answered anyone, without a key: the id, the address, the QUIC port,
  the certificate hash and the load of every node. It takes a key with `read`, as the
  documentation said.
- Hand-over: an `event` sent on the control channel while the node waited for the players'
  acknowledgements was taken and could be lost without a word to its sender. It is refused
  with `conflict: node_draining`.
- Data path: over WebTransport the node acknowledged a packet of a player it had taken out
  of the room and dropped it, so his client took a reliable message sent after a kick for
  delivered. Such a packet is not acknowledged, over QUIC either.
- Lobby: `lobby_update` told every watcher the name, the player count and the lobby props
  of a room the list does not offer him: a hidden one, a closed one, one whose last seat is
  held for somebody else. A watcher is told once, by the id alone and with `removed: true`,
  that a room left his list, and nothing of a room the list never offered him.
- Lobby: while Redis was unreachable, the node announced every changed room to the lobby
  watchers, a hidden or a closed one too. They are told of a room the list offers them.
- Rooms: a room whose creator was refused on the way in (player properties over the limit,
  for one) stayed empty under its name until the empty-room timeout. It is taken down at once.
- Rooms: `join_or_create` dropped `visible` and `open`, so a room meant to be hidden was
  listed and handed out by `join_random`. Both apply when the call creates the room.
- Rooms: `create` with `open: false` over the control channel refused its own caller with
  `room_closed` and left an empty closed room behind. The player whose call creates the room
  is let in; everybody else is still refused.
- Rooms: a room that was taken down after another one had been brought up under its name
  took that name with it, and the node stopped finding the new room among those it hosts.
- Multi-node: two players who came back to a handed-over room at the same moment lost it. One
  join adopted the room; the other had read its row a moment earlier, did not find the room
  hosted, took the row for a leftover and closed it. That player got `not_found`, and the next
  heartbeat threw the first one out with `left` (`rehost`). The join now takes the room the
  node has just brought up.
- Data path: the frames a client had sent over the tunnel before it read `left` were answered
  with errors: a kicked player got `conflict: session_replaced` and then `unauthorized: bad
  data ticket` for every frame. They are dropped now, and so is a control message for the room
  the session is no longer in, which gets `not in a room` instead of `session_replaced`. A
  session whose slot another session of the player took is still told `session_replaced`.
- Data path: a client that kept sending over a tunnel the node had given up got `unauthorized:
  bad data ticket` for every frame, 7507 errors in 4 s. The node now answers the first frame
  with `transport: data path closed: link dead` and drops the rest; a frame that is no ticket
  is answered once as well ([API](docs/api.md)).
- Data path: a frame the room refused (a malformed event, a lockstep command that is too
  long) reached the player as `error` only over the tunnel; over QUIC and WebTransport the
  node counted it and said nothing. It is answered on every transport.
- Data path: a player who kept sending frames the node cannot read got an `error` for every
  one of them. The node answers `frame_errors_per_sec_per_player` of them a second now (10 by
  default) and counts the rest in `net_errors_total`. A refused lockstep command or event is
  still answered every time.
- Lockstep: a command longer than 251 bytes reached no client, and of several that came to more
  than 255 bytes in one tick with their headers only the first ones did: the bundle carries the
  length of an input in one byte and cut the commands of the tick to 255 bytes, while the
  journal, the replay and the referee kept all of them. The sender heard nothing. The node now
  refuses a command of more than 251 bytes with `invalid_request`, and commands that do not fit
  a tick ride the next ones in order, the same for clients, the journal, the replay and the
  referee. More than 4096 bytes of commands waiting for a tick is refused with `rate_limited`.
  The documented 1 KB a command and 4 KB a tick never worked ([modes](docs/modes.md)).
- Lockstep: a late joiner, a player recovering from a desync and one far behind had the whole
  catch-up queued at once (537 KB after 5 minutes at 60 Hz), and one that took it slowly kept it
  on the node: eight joiners that acknowledged nothing grew the node by about 90 MB in 40 s.
  The catch-up now leaves a part at a time, each tick as many frames as keep the player's link
  under half of its 1 MiB, and the ticks made meanwhile follow in order ([modes](docs/modes.md)).
- Lockstep: a match whose last checkpoint was long ago, or that had none, never trimmed its
  journal. It now keeps `MAX_JOURNAL_TICKS` (4 hours at 60 Hz) in every case, and a join the
  journal can no longer serve is refused with `conflict` and a message that starts with
  `no_checkpoint`.
- Rooms: closing a room whose node did not answer was reported as done (`204` on
  `DELETE /v1/rooms/:id`, a `room.closed` webhook, `closed_rooms` of a deleted project) while
  the room went on, and a room of a deleted project stayed for good, out of the operator's
  sight. The close is now refused with `internal`, and every node closes the rooms of deleted
  projects it hosts with its heartbeat.
- Errors: the `message` of a `database`, `redis` or `internal` error carried what the driver
  or a library said (`sqlx`, `os error`, addresses of other nodes) to players and REST callers,
  and the node's log had no cause. The `message` is now the same every time, and the node
  logs the cause as an error with the route or the control message type and the project.
- Rooms: a node that took over a room of a silent node while it could not reach Redis started
  the room over, a lockstep match with another seed. The join is refused with `redis` and
  the room stays to be taken over with its checkpoint.
- REST, while the node could not reach Redis: `DELETE /v1/rooms/:id` and the operator's room
  close answered `204` for a room on another node and left it running; the operator's project
  page, `GET /v1/operator/rooms` and `GET /v1/operator/projects/:id/rooms` showed no rooms;
  `GET /v1/operator/attention` showed no rejections after waiting on every project in turn.
  They answer `500 redis`.
- Matchmaking: a pass that could not read the live rooms of a project seated its
  `fill_existing` tickets as if there were none. The pass is retried instead.
- Operator API: a project deleted while the node could not reach Redis lost its keys but kept
  its rooms running (`closed_rooms: 0`). The call now answers `500 redis` and deletes nothing.
- REST: `GET /v1/rooms` answered `200` with an empty list while the node could not reach Redis,
  as if the project had no rooms. It answers `500 redis` like the other calls.
- Lobby: a room in which nobody joined, left or changed anything for two minutes dropped out
  of the room list (`lobby_rooms`, `get_rooms`, `GET /v1/rooms`) until its next change, with its
  players still in it. A node now renews its rooms in the list with every heartbeat.
- Rust client and the C, C++, Godot and native Unity SDKs over it: `connect` and `join` waited
  without limit for the node's `authed` and `joined`. A node or a link that went silent after
  the control channel opened held the call, which blocks the game's thread in the C API, for
  good. Both waits now end with `timeout` after `connect_timeout` (10 s), like the wait for the
  data path.
- Web SDK: a call waiting for the node's answer (`connect`, a join, `rejoin`, a lookup) learned
  that the control channel had closed only from its own timeout, 10 s later; `rejoin` lost
  those 10 s on every URL that closed on it. The call now fails with `control channel closed`
  when the channel closes. A `connect` that failed after the socket opened left the socket
  open; it closes it.
- Unity SDK (managed client): the same in `ManagedClient`: `ConnectAsync`, the joins,
  `RejoinAsync` and the lookups waited out their 10 s timeout on a control channel that had
  closed, and a `ConnectAsync` that failed kept its socket. They fail with
  `control channel closed` as it closes, and the failed client is disposed.
- All SDKs: a send on a connection the game had been told was `closed` was accepted and kept.
  Nothing sent it any more, so a game that went on sending grew the queue without limit: 200 000
  reliable events held 53 MB in the Web SDK and 28 MB in the native core. The send now fails with
  `disconnected`, and the queue of the lost connection is dropped. The Web SDK and the managed
  Unity client also accepted sends outside a room, before a join and after `leave`, and held
  them until the next join threw them away; they refuse those too, as the native core did.
- All SDKs: a client the node took out of its room (kick, room closed, `session_replaced`,
  rehost) kept the room's data path and went on accepting sends. One reliable event sent after
  the `left` failed that client's next join (`conflict: session_replaced` in the Web SDK and the
  managed Unity client, `disconnected` in the native core over QUIC), and a few seconds later
  the client reported `closed` (`link dead`) with its control channel still open. The data path
  closes with the `left`, and a send fails with `disconnected` until the next join.
- Unity SDK (native client): `Client.Actor` and `Client.JoinedJson` went on describing a room
  after `Leave` and after the node took the client out of it. They are `-1` and `null` outside
  a room.
- Instant replay (`ReplayRing` in Rust, Web and C#): `window` returned a snapshot together
  with inputs that began several ticks after it whenever the oldest tick kept was not one with
  a snapshot, so a kill cam stepped the wrong inputs from the first tick on: with the documented
  `ReplayRing(300, 10)` that was every window asked for within the oldest ticks kept. The ring
  now keeps the inputs back to its oldest snapshot.
- Native core (C, C++, Godot, Unity, Unreal): `aurix_net_joined_json` named the room, the mode,
  the actor and the master, and left out the rest of what the node says at a join. A game had no
  list of the players already in the room and no room size or props: in Unreal `GetRoom().Players`
  was empty for the one who joined second and `MaxPlayers` was 0. It now carries `max_players`,
  `props`, `players`, `tick`, `seed` and `next_spawn_tag` as well.
- C++ and Godot SDKs: a sync world took a frame only through a client that was in a sync room,
  so a room's replay could not be played without one: `Sync::receive` threw `disconnected` on
  every frame, and `AurixNetSync.receive` raised no `spawn` or `change`. `Sync::receive(data,
  len)` and `receive(null, frame)` apply a frame with no client.
- Lockstep commands (all SDKs): `SendCommand` took up to 1024 bytes, but a tick carries at most
  251. A longer command left the sender without an error and reached nobody in Web, C# and
  Godot, and reached C and C++ games cut short, while the node's journal kept it whole. A command
  over 251 bytes (`MAX_COMMAND`) is refused at the sender.
- Client core (C, C++, Unity native, Godot, Unreal): what the node said while `connect` or a
  join waited for its own answer never reached the game. A game that left a room and joined the
  next at once got no `left`, and a notice, a player joining or a change of props sent during
  the join was lost. They are handed to the game after the call, in the order they came, before
  the room's cached events. Web and the managed C# client were not affected.
- Unreal SDK: a game was not told why it left a room: the plugin only cleared the room, so a
  kick, a closed room and a room moved to another node looked the same. `OnLeft(Reason, RoomId)`
  reports it. A `left` for an earlier room no longer clears the room joined after it.
- Unreal SDK: `GetRoom()` now carries the room's props, the lockstep tick and the next spawn tag
  from the join, and each player's props and spectator flag; `GetRoomPropsJson` and
  `GetPlayerPropsJson` were empty until the first change. `GetSeed` gives the lockstep room's
  seed, which the plugin did not expose.
- Unreal SDK: `JoinOrCreate`, `Connect and Join Room` and the NetDriver host (`Props=`,
  `LobbyProps=`, `Visible=`, `Open=`) take the room's props, the names of the props it publishes
  and its visibility, `JoinRandom` takes a filter expression,
  and `JoinOrCreateEx` / `Join or Create Room (Reserved Seats)` hold seats for the players of a
  matchmaking ticket and take the published names as a list of strings, as the other SDKs do.
- Unreal SDK: `SendCommand` checked for 1024 bytes; it refuses a command over 251 with the
  reason in `GetLastError`.
- Unreal SDK: a sync world took frames only from the room the player is in. `UAurixSyncWorld`
  with `bReplay` plays a room's replay frames given to `ReceiveReplayFrame`.
- Unreal plugin, session interface: Create Session made a room with no props, always visible and
  open, Find Sessions and Start Matchmaking ignored the search params, and the users of Start
  Matchmaking got no seats. Advertised session settings are now the room's published props and
  come back in search results, `bShouldAdvertise` is the visibility, Start Session closes the
  room unless `bAllowJoinInProgress`, Update Session sends the values, search params are the
  room filter, and seats are held for the `AURIXRESERVED` setting and the matchmaking users
  ([QuickStart](sdk/unreal/AurixNet/Docs/QuickStart.md)).
- Web and C# SDKs: their link gave the node up after 10 repeats of one packet, 3.3 s on a path
  with no round-trip sample yet, went on taking messages once it was dead and queued reliable
  messages without limit. It follows the rule of the node and the Rust client: lost when its
  oldest reliable packet has gone 15 s without an acknowledgement (`IDLE_TIMEOUT`,
  `Link.IdleTimeout`), and when more than 1 MiB of reliable messages wait for the window
  (`RELIABLE_BACKLOG_BYTES`, `Link.ReliableBacklogBytes`). The send that meets the limit fails
  with the reason and the game gets `closed` (`link dead`). The repeat timer is the core's too:
  one round-trip estimate for the link, 250 ms before the first sample, 60 to 1500 ms after; it
  was one per stream, 300 ms and 50 to 2000 ms.
- All SDKs: a game whose link the node had given up was not told. Over the tunnel a client
  that sent nothing reliable kept a data path the node no longer served, and over QUIC and
  WebTransport the reason read `data path closed` or `webtransport closed`. The node's answer
  `transport` to a frame of a tunnel it gave up, and its close of a QUIC or WebTransport
  connection with code 4, now end the data path with `closed` (`link dead`), and sends fail
  from there.
- Web SDK, Unity SDK (managed client): a link timer that came late found a reliable message
  15 s old and gave the link up (`link dead`) without resending it once, even with the node's
  acknowledgement already waiting to be read: a page that was busy for 15 s lost its room, and
  so did a hidden tab on the WebSocket tunnel, whose timers Chrome wakes once a minute, when it
  lost one message. The link is lost when a message that was resent has gone 15 s without the
  node's acknowledgement; one whose time ran out before any resend is resent and waits again.
- C++ SDK, Godot: a tick of `Lockstep` hands its commands over in `Event::commands`, with their
  senders and in the order every client gets them, and no longer among the players' inputs
  under actor 65535, where a game that makes an avatar for every actor of a tick made one for
  it. The C API hands the tick as the node sent it.
- Unity SDK: the package comes with the `.meta` files of its assets, written by Unity
  2022.3.62f3. Without them every project that took the package made identifiers of its own,
  and a package installed from a git URL or a registry, which the editor may not write into,
  had its scripts left out.
- Unity SDK: a player did not build in a project that holds only the packages this one names:
  `SyncTexture` uses `Texture2D.LoadImage` and `EncodeToPNG` of the image conversion module,
  which the package did not name among its dependencies.
- Unity SDK: the editor refused to load an assembly after its RPCs were woven (`Unable to
  resolve reference 'System.Private.CoreLib'`): the weaver took `object` and `Type` from the
  runtime it runs in. It takes them from the core library the woven assembly references.
- Unity SDK: the package did not compile in an editor: its weaving assembly uses Mono.Cecil
  and the package did not depend on `com.unity.nuget.mono-cecil`, which brings it.
- Unity SDK: `NetRigidbody` and `NetworkRigidbody` read a body's velocity by the name the
  editor has (`linearVelocity` since Unity 6), and the scene's objects are found with
  `FindObjectsByType`: the editor has no deprecated call to warn about or rewrite.
- Unity SDK: the assembly that weaves `[ServerRpc]`, `[ObserversRpc]` and `[TargetRpc]` was
  named `Aurix.Net.CodeGen`, and the editor runs a post-processor only from an assembly named
  `Unity.*.CodeGen`. It is `Unity.Aurix.Net.CodeGen`. The package names the engine modules it
  uses (animation, audio, IMGUI, physics), and `BuildCheck~/unity-project` builds the package
  in an editor from the command line and checks that the RPCs of a probe were woven.
- Web SDK, Unity SDK (managed client), Rust client: `rejoin` after a rehost asked the last join
  again. A client that a random join had brought into its room was refused (`not found: no
  matching room`) or would have been matched into another room, and after a `join_or_create`
  the Web and C# clients sent a `join` that still carried `mode`, `max_players`, `props`,
  `reserved` and `lobby_props`. The rejoin is a `join` by the name of the room the client was
  in, with the player props it entered with.
- Web SDK: a client that sent nothing for 90 s was disconnected (`closed` with `control channel
  closed`), in a quiet room as in a menu: the node closes a control channel it hears nothing
  from, and the Web client probed it only when the game called `ping()`. It probes once a second
  by itself now, as the Rust and C# clients do; `rttMs` is measured from the connect on, and a
  sync session no longer sends probes of its own.
- Unity SDK (managed client): the answer to a round-trip probe reached the game as a `pong`
  control message, and was not measured, when it came after the next probe had gone out: on a
  link slower than a second. Every probe in flight is told from a game's own `ping` now, and the
  round trip is taken when the answer is read, not when the game polls.
- Unity SDK (managed client): the link was worked only inside `Poll`. A game that did not call
  it for 15 s, as during a scene load, acknowledged nothing, so the node gave the link up and
  the game lost its room (`closed` with `link dead`). The client now reads the socket,
  acknowledges, resends and probes the control channel on its own tasks, off the game thread;
  `Poll` only hands the game what has arrived, on the thread that calls it.
- Unity SDK (managed client): what the node sent was kept for `Poll` without a limit, so a game
  that did not poll grew by all the room sent it. The client holds 4096 events
  (`ManagedClient.EventBacklog`), as the Rust client does, and stops reading while that many
  wait; a link left unread for 15 s is lost by the node's rule.
- Unity SDK (native client): `Client.JoinOrCreate` had no `lobbyProps`, so a room it created
  published no props, and neither a room list nor a random join with an `expr` found it. It
  takes them as the managed client does.
- Unreal SDK: `SendEventEx` sent an event for `EAurixNetChannel::ReliableUnordered` on the
  reliable ordered channel. It goes on the channel it is given.
- Lobby: a node that lost its Redis connection heard no room changes for about 2 s, and a room
  closed in that time stayed in the lists of its lobby subscribers until they subscribed again.
  When the node is listening again it sends them `lobby_rooms`, the whole list, once more.
- Lobby: a room closed while its node could not reach Redis stayed in the room list
  (`lobby_rooms`, `GET /v1/rooms`) for up to 2 minutes after Redis was back, and a room created
  in that time was missing from the list until its next change. The node takes the first out
  and writes the second as soon as it is listening again.
- Node: a WebSocket handshake cut off before the node answered kept its place in the count of
  connections from that address (`limits.max_sessions_per_ip`) until the node restarted. Enough
  of them, and nobody behind that address could connect: `429 too many connections`. The place
  is now given back however the connection ends.
- Web and Unity SDKs: of two `rooms`, `findFriends` or `lobbyStats` calls in flight at once the
  second got the answer to the first, and in Unity the node's refusal of one call failed every
  call in flight. Each call goes out under an id of its own now and takes the answer or the
  refusal that repeats it; a refusal with an id no longer fails a join in flight either.
- Rust client: `stats` and `find_friends` take the `id` their answer comes back with, as `rpc`
  does. They sent none, so a game could not match an answer or a refusal to its request.
- Sync: a frame repeated after its tick, with no input the room had not taken, was counted as
  late in the input report (`0x25`) and in `net_sync_late_inputs_total`, so a client that
  repeats its last frame raised its input lead for nothing. Such a frame is left out of both.
- Sync: of a frame whose inputs were all for ticks already simulated the room ran the first new
  input and left the others to the repeats in later frames; when that frame was the last one a
  client sent, its last writes never reached the room. The inputs of such a frame run on the
  ticks that follow, one a tick, in the order sent. An input sent for a tick that a late input
  already held replaced it; it runs after it now.
- C, C++, Godot and Web SDKs, sync rooms: the last write, input or op before a pause went out in
  one frame only, while every earlier one rode in three. When that frame was lost or came late,
  the room kept the value before it for good. The client sends the last frame again on each of
  the next two room ticks now.
- Unity SDK, sync rooms: `NetRunner` followed the last write, input or op with two frames that
  carried an empty input for the next ticks, so a room with logic ran the empty input instead of
  holding the player's last one. It sends the last frame again on the next two room ticks, as
  the other SDKs do.
- C, C++ and Godot SDKs: a client that asked one node of a region for a room hosted on another
  got `room_elsewhere` and had no call to follow it, unlike Rust, Web and C#.
  `aurix_net_connect_and_join`, `Client::connectAndJoin` and `connect_and_join` connect, join
  and move to the room's node in one call.
- Unity SDK: `RpcAsync` took the first `rpc_result` to arrive, so of two calls in flight the
  second returned the answer to the first. Each call waits for the answer with its own id now.
- A node started with port 0 in `server.http_addr` registered itself in the fleet under port 0
  and logged that address. It registers and logs the port it was given. The node tests start
  their nodes this way now instead of picking a free port in advance, which another process
  could take first (`Address already in use`).
- Matchmaking: filing a ticket read the waiting tickets of every project. One filing took 19 ms
  with 1000 tickets waiting in another project (3 ms with none), and with a few thousand waiting
  the node answered `500` with `redis: timed out`. Waiting tickets are kept per project now and
  filing reads only the ticket its player filed before; `GET /v1/matchmaking` reads only its
  project. While a deployment is being upgraded, a ticket filed on a node of the other version
  than the one that matches waits until the upgrade is over.
- Matchmaking: the Redis keys of one project (its tickets, waiting set, players' last tickets
  and hourly counters) carry the project id as a hash tag, `net:mm:{<project>}:...`, so the keys
  a script touches are in one Redis Cluster slot.
- Matchmaking: tickets one player filed at the same moment all stayed waiting, so the player
  could be matched against himself. The last one filed stays and the others are cancelled.
- Sync: a room continued from a checkpoint started its logic module with seed 0
  (`aurix_init(0)`) instead of the seed the room was created with. The seed is in the checkpoint
  now.
- A room continued on another node without a checkpoint always got the same seed: 1 in
  lockstep (the `seed` of `joined`), 0 in sync. It gets a random one below 2^53, as a new room
  does.
- A node that was stopped exited without the webhooks it still owed: an event queued moments
  before, or a delivery waiting for its retry, was lost. A match made by a node about to stop
  could reach nobody's backend. The node now finishes the matchmaking cycle under way, makes no
  further matches, and delivers what it owes before it exits.
- A node told to stop waited without limit for the requests under way: one that never ended
  held it until it was killed. Stopping now has one limit, `server.shutdown_secs` (default 8),
  for the requests under way, handing the rooms off and the webhooks owed; when it runs out
  the node logs what was left and exits.
- Sync: the room checkpoint did not say which entity was a player's avatar. A room continued on
  another node took the first entity the player owned, which could be one the player had
  spawned. The avatars are in the checkpoint now.
- Sync: the ownership policy of an entity (`orphan`, `to_master`, `dynamic`) was not part of the
  room checkpoint. After the room moved to another node, an entity meant to outlive its owner
  was despawned when the owner left. The policies move with the room now.
- Sync: a delta built before the client's data path was open counted as sent. An entity that
  did not change afterwards, the client's own avatar among them, never reached a client that
  sends no acks. The first delta a client gets now holds everything it should see.
- Lockstep: the room seed in `joined` was a nanosecond timestamp, more than a JSON number keeps
  (2^53), so a client reading it as a double started from a different seed than the node. The
  seed is a random number below 2^53 now.
- Sync: an input that reached the room after its tick was simulated was dropped, and its ops
  with it: a spawn sent right after a short stall of the client never happened. Such an input
  runs on the room's current tick now, once and in the order sent.
- Console: a chart covering one to two days labelled its time axis with the time alone, so the
  same hour appeared twice with no date; the labels carry the date now (`Nov 17, 04:00 PM`).
- Limits: a refused value did not say which limit it belonged to (``invalid value: integer
  `100000`, expected u16``); the message names the field now, for the limits form and for
  limits already stored that the node cannot read.
- Lockstep: a room with a referee numbered its state hash one tick ahead of the clients, so a
  client reporting hashes the way `docs/sdk.md` says was called desynced every time. The state
  after tick T is hashed under T on both sides now; checkpoint ticks are unchanged.
- Console: the project page counted a megabyte as 1 000 000 bytes and the rest of the console as
  1 048 576, so the month's traffic read 196 MB there and 186.5 MB in billing.
- Console: the matchmaking page crashed as soon as a ticket was filed; it took the ticket state
  for a string, and the node sends an object.
- Console: the matchmaking page filed every ticket for 8 players whatever size the form showed;
  it sent the size in a field the node does not read.
- Unreal plugin, rollback: `AurixRollbackRunner` answered a checkpoint request with the state
  after tick N under the number N, and after a restore at N went on from N + 1. The node
  sends the inputs from N on after a checkpoint, so the number means the state before N. Two
  Unreal clients cancelled each other's error; in a room with a client on another SDK the
  late joiner missed one tick or ran it twice and the room reported it desynced on every
  hash. A restore with an empty state (no checkpoint yet) was handed to the game as bytes to
  load; the runner now goes back to the state it started with. Checked in UE 5.8.2 against a
  Web SDK player, joining either way: 0 of 29 state comparisons matched before, 26 of 26 after.
- Unity framework: with adaptive sync on, `NetworkTransform` on a receiver ran past the place
  where the object stopped and stayed there (0.6 units ahead at 3 units/s with the default
  level). The stop was sent as one sample, and the receiver kept extrapolating from the last
  two. The controller now sends a second sample at rest.
- SDKs, data path: the Rust client and the Web SDK sent the ticket of a QUIC or WebTransport
  path as a single datagram, so a lost one ended the join after the timeout. They now repeat
  it every 100 ms until the node's first datagram arrives. With 10 % of UDP packets lost 6 of
  40 joins failed in the Rust client and 7 of 40 in Chrome; now 0 of 40 in each.
- Data path: when a WebTransport session ended while the player stayed in the room, the node
  kept the dead path on the player's slot, and room events went nowhere instead of falling back
  to the control channel as they do after a QUIC connection ends.
- Data path: when a player joined again from a second connection, the data ticket of the
  replaced session still opened a QUIC or WebTransport path, took the data of the new session
  and cut its link. The node now refuses it with `not joined`, as the WebSocket tunnel did.
- Lockstep: the state a desynced client reloads from went out as `Catchup` (`0x24`), the frame
  of a late join, and a client that drops a `Catchup` behind its own ticks never recovered. It
  is now a frame of its own, `Restore` (`0x2A`), with the same layout. Clients have to handle
  `0x2A`; one that does not skips it and stays desynced.
- Modules: creating a room compiled its module on the node's runtime when the node had not
  compiled it yet (after a restart, or on another node than the upload). On a node with one
  runtime thread every player waited for the compile, seconds for a large module. The compile
  now runs on the blocking pool, as it does on upload.
- Modules: the node kept every module version it had ever compiled, so a project that
  replaced its module over and over grew the node's memory without limit. The compiled-module
  cache is now bounded (64 MiB of module bytes, least recently used first).
- Data path: the node closed a QUIC or WebTransport connection with `bad ticket` when any
  datagram reached it before the ticket, which happens whenever the ticket datagram is lost.
  It now drops such datagrams and waits 5 s for a valid ticket. A ticket repeated after the
  path is open used to be parsed as a packet; it is now ignored, so a client can repeat the
  ticket until the node answers.
- Limits: a project whose stored limits did not parse ran with no limits at all. The node now
  refuses its new rooms and joins with `quota_exceeded` until an operator sets the limits
  again, lists the project under `invalid_limits` in `GET /v1/operator/attention`, and returns
  `limits_error` in the project detail. Rooms that are already running keep running.
- Lobby: a client that subscribed to the room list more than once got every update as many
  times as it had subscribed, until it unsubscribed. A session is now subscribed once.
- Operator API: `POST /v1/operator/projects/:id/limits` stored any body and answered 204. One
  value of the wrong type or out of range, such as `max_players_per_room: 100000`, made the
  whole object unreadable, and the project then ran with no limits at all, the monthly caps
  included. Such a body, and one with an unknown field, is now refused with `invalid` and the
  limits stay as they were; an unknown project answers 404 instead of 204.
- Billing: `rooms_created` in `GET /v1/operator/billing` was counted from the rooms table,
  where a closed room's row is deleted after `closed_rooms_days` (7 by default), so the report
  for a finished month showed only the rooms closed in the last week. A created room is now
  counted in the usage table, which is kept for `usage_days`, and shows as the `rooms_created`
  metric in `GET /v1/usage`. Rooms created before this version are not in the count.
- Lockstep: a client stopped getting ticks for good once three bundle datagrams in a row were
  lost, because the room only ever repeats the last three bundles and the client had no way to
  ask for an older one. With 10 % of packets lost it happened within 4 to 20 seconds in 6 runs
  of 8. A client can now tell the room which tick it needs next (`Ack`, `0x29`), and the room
  sends it every bundle from that tick on, over the reliable channel when they no longer fit a
  datagram; 6 runs of 6 then reached the end. Clients that send no `Ack` are served as before,
  so each SDK has to send it to get the fix.
- SDKs, lockstep: with real packet loss the reliable catch-up of a room past tick 0 could reach
  the client after its first bundles. The client had started from those bundles, went back to
  the catch-up and asked for ticks the room, which keeps the highest acknowledgement, no longer
  resent. A client now starts at tick 0 or at the catch-up and ignores a catch-up behind the
  ticks it already has. Chrome over WebTransport with 10 % loss: 1 run of 8 stalled from the
  start, now 0 of 8.
- SDKs, lockstep: the Rust, Web and C# clients go back to the state of a `Restore` (`0x2A`)
  whatever they have delivered since; C++ and Godot get it through the Rust client. Web, three
  clients, one reporting a wrong hash from tick 60 on: with the rule above it was never
  restored and the room saw a desync at every hash, 30 in 12 s; now it is restored at the
  first one. A late joiner whose `Catchup` comes 400 ms after its first bundles still gets
  every tick once, 3 runs of 3.
- SDKs, sync: an input sent before the client's first delta was stamped from tick 0, so a room
  that was already running refused it as late and a spawn, despawn or hand-off in it was lost
  for good. The Rust, Web and Unity `NetRunner` clients now hold their inputs until the first
  delta. Web under CPU load, a spawn as the client's first input: lost in 4 runs of 40, now 0
  of 40; C++: 4 of 30, now 0 of 20. The refused input also raised the input lead to its limit:
  a Web client that wrote from the moment it joined a room 3 s old had its inputs applied a
  second late for 2.5 s; now they stay 3 to 5 ticks ahead.
- All SDKs: a client went on describing the room it had left. After `leave` the actor id, the
  room facts (`joined`, `aurix_net_joined_json()`, `JoinedJson`, `RoomMode`) and the seed were
  still those of the old room, though the C header promises NULL outside a room. They are
  cleared on `leave` now: actor -1, no room facts, no seed.
- All SDKs: a client the node took out of its room (kick, room closed, the same player joining
  from a second connection, rehost) went on describing that room. `left` names the room now, and
  a client clears its actor id, room facts and seed when `left` is about the room it is in. A
  late `left` about a room it left earlier changes nothing.
- Unity `LockstepDuel` sample: the state hash went out under the number of the next tick, not
  of the tick it was computed after, which is the rule `docs/sdk.md` gives and the other
  clients follow, so the sample and a client of another SDK reported one state under two
  numbers. The managed live check reported its hash the same way.
- Unity framework: a message over 65535 bytes (an RPC with a 70 KB `byte[]`, for one) broke the
  framing of the outgoing buffer: `NetworkUpdate` threw `ArgumentException` on every call from
  then on and nothing else was sent. Such a message is now refused where it is sent, with a
  `NetException` that names its size; large payloads go through `SyncBigData`.
- SDKs, lockstep: a client that lost more than three bundle packets in a row stopped getting
  ticks for good. The clients now acknowledge every bundle frame (`0x29`), so the room resends
  from the tick they need. With 10 % of packets lost a client stalled in 5 runs of 8 in the Web
  SDK, 4 of 8 in the managed C# client and 5 of 8 in the Rust client; with acknowledgements, in
  0 of 8 each. `aurix_net_lockstep_receive`, `aurix::net::Lockstep::receive` and Godot's
  `AurixNetLockstep.receive` take the client the acknowledgement goes through, as the sync
  helpers do; `LockstepClient::ack_frame` builds it in Rust.
- Unity framework, prediction: the owner of a `PredictedBehaviour` simulated a fixed
  `inputLeadTicks` (3) ahead of the newest server tick, so with a round trip longer than three
  ticks every input reached the server after its tick and was dropped: behind 100 ms each way
  the server missed 87 inputs of 90 and the object never moved. The lead is now the measured
  round trip (`statistics.ping`) plus `inputLeadTicks` of slack: 0 missed, server and owner agree.
- Rooms: the refusal for a full room read `room is full (0 players)` whatever the room's size.
  It now names the limit: `room is full (max_players is 4)`. The code is `room_full` as before.
- Lockstep: when an input frame arrived late, the room put the oldest input it repeated into the
  next bundle instead of the newest, so the fresh input was lost and one the room had already
  confirmed could be confirmed again. With 50 ms of delay each way a client that sent 594 inputs
  had 12 lost, 10 confirmed twice and 6 out of order; now 1 lost and none twice or out of order.
  Only the newest input of a late frame is moved to the next tick.
- Usage: traffic a player moved after the node's last metering pass was never billed once he
  left, so a session shorter than a minute counted no `bytes_in` or `bytes_out` at all and every
  longer one lost its tail, in `/v1/usage`, the billing report and the monthly byte cap alike.
  The node now adds it when the data session closes.
- SDKs: a client that left a room kept its data path running: it went on acknowledging and
  resending the room's reliable traffic. Over a real network the node got those frames after the
  leave, answered each with an `unauthorized: bad data ticket` error, and the next `join` on the
  same client failed with it (or its first reliable events were lost). `leave` now closes the data
  path in the Rust, Web and managed C# clients.
- SDKs, lockstep rooms: with a round trip of about 130 ms and more every input arrived late, and
  the room then confirmed some inputs twice and dropped others (36 of 581 lost at 200 ms). The
  input delay could not grow past 8 ticks, less than such a round trip takes at 60 Hz. It now
  goes up to 32 ticks, covers the lateness of a report in one step and gives the excess back
  after three reports with ticks to spare; a report that still covers inputs stamped before the
  last raise does not raise it again. Inputs sent before the first frame from the room were
  stamped from tick 0 and arrived late by the age of the room; `send_input` now sends nothing
  until the room's tick is known. `on_input_stats` (`onInputStats` in the Web SDK) takes the
  report's tick as its first argument.
- SDKs: the answers to the round-trip probes the clients send once a second reached the game as
  a `pong` control message every second. The clients keep them to themselves; a `ping` the game
  sends still gets its `pong`. The Web `Client` has `ping()` and `rttMs` for it.
- SDKs: a room made with `joinOrCreate` could not be found by the `expr` filter of `rooms` and
  `joinRandom`, and its props were missing from room listings: no SDK could name the props a room
  publishes. `joinOrCreate` takes them as `lobbyProps` in Web, C# and C++ and as `lobby_props` in
  Godot.
- C, C++ and Godot SDKs: when the reliable link died while the control channel stayed up, the game
  was never told: no `closed` event came and reliable sends kept returning success (20 seconds
  and 200 sends in the check). The Rust client now reports `link dead`, and reports one lost
  connection once, as the Web and C# clients do.
- Unity, Godot and C++ SDKs: a late joiner got neither the cached relay events nor the chat
  history, as in the Web SDK below. The Rust client hands the cached events out as data events
  right after the join, so C, C++ and Godot receive them like any relay event; the managed C#
  client raises them through `OnEvent` (`EventFrame.Cached`, `ManagedClient.Cached`). `Chat` in
  C++ and `ChatClient` in C# deliver the history of a channel when it is joined.
- Web SDK: a late joiner got neither the cached relay events nor the chat history. The node sends
  them in `joined.cached`, and the client dropped that field. They are now emitted as `event` after
  the join (`cached: true`, also kept in `Client.cached`), and `Chat.join` delivers the history of
  the channel.
- Web and Unity SDKs: one lost connection was reported more than once. The Web client fired
  `closed` up to three times (the link, the WebTransport session, then the control channel), so a
  handler that rejoins opened three connections for one player; the managed C# client raised
  `OnClosed` on every `Poll` once the link was dead. It is now reported once per connection.
- Web and Unity SDKs: `connectAndJoin` and `rejoin` left the control connection open when the join
  failed, and the game had no way to close it. It is now closed before the error is returned.
- SDKs, lockstep rooms: an input was confirmed more than once when the game did not send one every
  tick (5 inputs sent 200 ms apart in a 30 Hz room came back as 12). Earlier inputs were resent
  under new tick numbers; they are now resent only when the ticks follow each other.
- SDKs, sync rooms: with a round trip of about 100 ms and more nothing a client wrote reached the
  room, so other players saw neither its entities move nor the ones it spawned. Inputs were stamped
  two ticks ahead of the last delta whatever the latency, and the room drops an input whose tick
  it has already simulated. The clients now measure the round trip with the control `ping` once a
  second and stamp inputs ahead by it (`rtt_ms` in the Rust client, `RttMs` in the managed C#
  client). A spawn, despawn or ownership op could also be applied twice: earlier inputs were
  resent under new tick numbers when the ticks were not consecutive. They are now resent only
  under their own ticks.
- Relay: a host that sends every client its own events (the Unreal driver sends one per engine
  packet) ran into the per-player event limit with two or three clients, and the node dropped
  the rest. The limit is now counted per receiver, see Changed.
- Relay: an event over the limit sent as a JSON `event` message closed the player's connection,
  and one sent on the data path was answered with `invalid`. Both are dropped with
  `rate_limited` and the connection stays, as documented.
- Unity SDK: a room named `actor` gave every player the actor id -2147483648, and a player whose
  display name is `data` could not join and blocked the room for everyone after him. The managed
  client searched the control message text for a field name and took the first match, even inside
  a value. Control messages, the region list and the bot kit are now read with Newtonsoft.Json
  through Unity's `com.unity.nuget.newtonsoft-json`; the bot kit used `System.Text.Json`, which
  Unity does not ship. `BtNode.Parse` takes a `JToken`.
- Unreal plugin: first build and run with an engine (UE 5.8.2, Linux). `RunUAT BuildPlugin` passes
  for the editor and the game (Development, Shipping), the packaged plugin loads, and a listen
  server with two clients goes through login, replication and host migration. Before this the
  plugin did not compile (13 errors) and could not connect once it did:
  - travel URLs: `aurix://<room>` is not a URL the engine accepts. A host now opens
    `<map>?listen?Room=<room>` and a client opens `<room>.aurix`; session connect strings and the
    host-migration travel use the same forms;
  - the listen server runs the engine's stateless handshake before it creates a connection;
  - a connecting client finds the subsystem through its world context (its driver has no world yet);
  - the packet size follows the engine cap (`MAX_PACKET_SIZE`), and the plugin's `Config/Engine.ini`
    carries the timeouts and rates the engine ships only for its own drivers;
  - host migration reads `master_changed{actor}` and keeps the server connection open until the
    travel, so the engine does not drop to the default map;
  - `build_native.sh` stages `libaurix_net.so` next to the module binaries on Linux, so a packaged
    plugin loads.
  Still beta: other engine versions and platforms are not built.
- Node: when an offline player's slot expired (`player_ttl_secs`, 30 s by default), the sweep
  removed the sessions of the players who stayed instead of the one who left. The rest of the room
  got `session_replaced` and `bad data ticket`, and `player.left` webhooks named the wrong actors.
  Present in 0.1.0.
- Node: a banned player (by id, IP or device) and a player of a suspended project are refused on
  every way into a room. Before this the checks ran only on `create`, so `join` let them in.
- Node: `rejoin_actor` no longer skips the `join.validate` webhook and the concurrent player cap
  for a player who holds no slot in the room.
- Node: a grant limited to one room is held to it by `create` and `join_random` too, not only by
  `join` and `join_or_create`.
- Node: after a kick the player's next command no longer fails with `session_replaced`; after a
  room is closed its players can create or join another room, and their disconnect no longer puts
  the closed room back into the room list.
- Rooms: `set_master` refuses a spectator (`forbidden`), so a spectator cannot end up kicking
  players or changing room properties.
- Node: a player who joins the room again with `spectator: true` keeps the player role in sync
  and lockstep rooms. Before this their inputs were dropped for the rest of the match.
- Node: a node that was silent for three heartbeats (a stalled host, a database outage) and then
  carried on kept hosting rooms the fleet had already put up for adoption, so a join through
  another node started a second copy of the same room. With every heartbeat a node now takes back
  its rooms nobody adopted and drops the ones another node runs (`left` with reason `rehost`).
- Matchmaking: a group that starts short of `max_players` now needs to be large enough for every
  member's `min_players`, and waits for its own oldest ticket's `max_wait_secs`. Before this both
  values were taken from the oldest ticket of the whole queue, so a player who asked for four was
  started in a room of two. A player who wants more than there are stays in the queue.
- Operator: a node counts as stale after it misses two of its own heartbeats, not after a fixed
  30 s. With `server.heartbeat_secs` above 30 a healthy node was listed in `stale_nodes`, raised
  the `node.stale` alert and showed as stale in the dashboard, where its buttons were disabled.
- Matchmaking: the `match.found` webhook for `fill_existing` tickets seated into the same running
  room in one pass named only one of the players next to all the ticket ids.
- Multi-node: a room on a node that registered within the last heartbeat is found from the other
  nodes. Before this a join by name through a node that had not listed the host yet answered
  `not_found` and closed the room's record, so nobody could reach the running room by name.
- Multi-node: a notice, a kick, a replay request and `join_random` reach a room whose node
  registered within the last heartbeat. Before this they answered `hosting node unknown` or
  `no matching room` until the node list was next refreshed.
- Multi-node: `DELETE /v1/rooms/{id}` with a project key closes the room on the node that hosts
  it. Sent to another node it used to close only the room's record: the players stayed in a
  room that no listing showed.
- Matchmaking: a ticket cancelled while a matching pass was running could be matched anyway. The
  cancel answered 204, the ticket then read `matched`, and the other players of the room waited
  for someone who had left. A ticket now leaves `searching` once: whichever of the cancel and the
  pass comes second sees that and backs off, and a room that lost a player this way is not
  started; its other tickets are matched again on the next pass.
- Bans: a ban request the node refused with 400 (unknown `scope`, or an address or device ban
  for a player who never connected) banned the player by id all the same, with no audit entry.
  Nothing is written now unless the whole request is accepted.
- Webhooks: a webhook created through the project API got no events for up to 30 seconds if the
  project had already produced one, and a deleted webhook kept being called for as long. Both,
  and a rotated secret, take effect at once on every node: the node that handled the request
  tells the others through Redis.
- Webhooks: a failed read of a project's webhook list is no longer remembered as "no webhooks".
  After a short database outage the project's events were dropped for up to 30 seconds with
  nothing in the log; the node now warns and keeps the list it had.
- Webhooks: `player.left` carries `"kicked": true` when the room's master kicks a player, as
  documented. Before this only a kick through the REST API was marked, so a backend could not tell
  a master's kick from a player leaving.
- Web and Unity SDKs: a reliable burst longer than 32 packets (a message over about 35 KB, or many
  reliable sends in one frame) no longer ends the connection when one packet of it is lost. The
  send window counted unacked packets instead of the span of their sequence numbers, so the node
  could not acknowledge the retransmit and the client reported `link dead`.
- Web and Unity SDKs: a client that leaves a room can join another one without reconnecting. The
  second join reused the link state of the first room: in Web it failed with `data path timeout`,
  in Unity it returned normally and reliable messages were then silently lost.
- C++ SDK (and the copy the Godot extension builds with): a moved `Client` keeps its callbacks.
  Moving one, for example into a `std::vector`, dropped `onData`, `onControl` and the rest, so the
  game stopped receiving anything without an error.
- Web SDK: `client.on("notice", ...)` compiles in TypeScript. The event was emitted but missing
  from the type of `on`.
- Web, C++ and Godot SDKs: a sync room takes one input per tick, and the SDKs now send it that way.
  A game that sent writes every frame in a slower room (60 frames a second, 20 Hz room) had them
  stamped further and further ahead, so other players saw it seconds late and the last writes never
  arrived. The C ABI gains `aurix_net_sync_pump`; the C++ wrapper and the Unreal plugin call it
  for you. In Unity the same holds for `NetRunner.SendInput`, and input ticks start over in every
  room.
- Web, Unity, C++ and Godot SDKs: lockstep `sendInput` sends one input per room tick. Called more
  often (every frame at 60 frames a second in a 30 Hz room) it stamped inputs further and further
  ahead until the node dropped them, and the player's input stopped reaching the game. Extra calls
  now send nothing and return -1 (`None` in `net-client`).
- SDK docs: the Godot and C++ lockstep examples labelled a checkpoint with the tick from the
  request, so a late joiner desynced; they now use the simulation's own tick. The Godot README no
  longer points to a `rejoin()` that does not exist, and the event codes taken by sync and lockstep
  rooms are stated as they are (`0x10–0x2F`, in those rooms only).
- Godot SDK: the `event` signal no longer fires for sync and lockstep frames in rooms of those
  modes. Only relay events reach it, as in the Web SDK.
- Webhooks: `desync` can be named in a webhook's `events`. The node sent it only to webhooks
  without an event filter and refused the name as unknown.
- Node: `create` on the control channel holds the seats listed in `reserved`, as REST room
  creation and `join_or_create` do. The field was ignored there.
- Console: the nodes, rooms, projects, keys and modules tables sort by a column when its header
  is clicked. The columns were marked as sortable, but no page turned the sorting on. Date columns
  sort by time.
- Load test: `aurix-net-loadtest --mode sync` stamped its inputs two ticks ahead of the last delta
  whatever the latency, so over a link with 50 ms of delay each way the room dropped every one
  of them. It now measures the round trip with `ping` once a second and stamps that far ahead.
- Unreal plugin: a room name, kick reason, RPC name or room filter with a line break or another
  control character no longer produces a message the cell refuses; the subsystem writes its
  messages with the engine's JSON writer instead of pasting strings. An RPC result whose body is
  an array reaches `OnRpcResult`; it used to arrive empty.
- Unreal plugin: `Disconnect`, a new `Connect` or closing the game during an async join freed the
  client the join was still using; it is now freed after the join returns. A `Disconnect` during
  an async connect is no longer undone when the connect finishes.
- Unreal plugin: the NetDriver flushes its packets at the end of the frame; they used to wait for
  the client's own 5 ms timer.
- Unreal plugin: an `AurixSyncActorComponent` whose `AurixSyncWorld` gets its schema after
  `BeginPlay` now receives the world's events. Before, its spawned entity was never bound and
  remote changes were not applied.
- Unreal plugin: `AurixPhysicsPredictionComponent` with `BodyId` left at 0 derives the id from the
  actor's name text. It used the hash of the engine name, which differs between processes, so
  players ignored each other's snapshots of the same body.
- Unreal plugin, session interface: `Logout` logs the user out; the status stayed `LoggedIn` and
  the id stayed valid. `GetUniquePlayerId` no longer creates a user as a side effect, and `Login`
  reports the status change once. The first local player is logged in at start, as with
  OnlineSubsystemNull.
- Unreal plugin, editor: the debugger's Request rooms and Request stats buttons show their answers
  (the subsystem keeps them: `GetLastRooms`, `GetLastStats`), and the Settings button opens the
  plugin's section.
- Unreal plugin: the `aurix.net.rooms` console command prints the room list once. Each run used to
  add a log handler that stayed, so later lists were printed several times.
- Unreal plugin: `AurixSyncActorComponent` can bind the entity numbered 0. It used 0 for "no
  entity", but the cell numbers entities from 0, so the first entity of a room could not be bound
  or owned and its changes were applied to every unbound component.
- SDKs, sync rooms: the input lead followed only the round trip measured once a second, so when
  the latency rose it stayed short for many seconds and the room dropped every input as late:
  after a step from 20 to 120 ms each way 462 of 736 input frames in the Web SDK and 438 of 720
  in the C++ wrapper, nothing reached the other player. The SDKs now read the room's input margin
  reports (`0x25`) and correct the lead by them: 70 and 40 frames, all right after the step. The
  report is taken by the sync helpers (`SyncSession`, `aurix_net_sync_receive` returns 2,
  `aurix::net::Sync::receive` returns true, `NetRunner`) and shares its controller with lockstep
  (`MarginControl`).
- Console: the quota form of a project and the matchmaking page showed nothing when the node
  refused the request. A quota out of range, which the node now answers with `invalid_request`,
  looked like a button that did nothing. Both show the node's message.
- Console: a project whose stored quotas the node cannot read is listed under "Needs a look"
  on the overview, and its page says that new rooms and joins are refused and shows the node's
  error, until the quotas are saved again.
- Unreal plugin: `JoinSession` told a full room from a missing one by words in the error text.
  It reads the error code of the C API now (`aurix_net_last_error_code`): `SessionIsFull` for
  `room_full`, `SessionDoesNotExist` for `not_found`. `UAurixNetSubsystem::GetLastErrorCode`
  and the async Blueprint nodes give the same code as `EAurixNetError`.
- Unreal plugin: travelling to a room froze the game until the node answered the join, and a
  host migration waited a fixed 0.2 s on the new host and 1.5 s on the others. The driver joins
  the room in the background and holds the engine's handshake back until it is in, as the
  engine's own drivers do with a socket that is still connecting; a refused join ends the
  travel with a network failure. With two clients, the one that stays a client was back on the
  new host 0.70 s after the old host left, 2.17 s before.
- Console: while the node could not reach Redis, the overview said "All quiet" under "Needs a
  look" and counted no rooms, the rooms page went back and forth between the error and "No live
  rooms", and a project page showed a spinner for ten seconds before the error. All three show
  the node's error with Retry from the first failed request, and keep it on screen while they
  repeat the request, until one succeeds.
- `wasmtime`, which runs game modules on the node, goes from 50.0.1 to 50.0.2. RustSec published
  eight advisories against 50.0.1 on 16 November (RUSTSEC-2026-0566 to 0573); one of them lets a
  Wasm guest abort the host process (RUSTSEC-2026-0568).

### CI
- `ci.yml`: the Unity job fails when an asset of the package has no `.meta` or a `.meta` has
  no asset.
- `ci.yml`: a new job, `sdk-live`, runs the live checks of the Web, C#, C++ and C SDKs against the
  node the `rust` job built and through the four proxies of `sdk/cpp/README.md`. Until now CI
  built these checks and skipped their live part. With `NET_LIVE_REQUIRED` set, as in this job, a check
  fails where it would skip (no key, no proxy, no game module, no `libaurix_net`).
- `ci.yml`: `sdk-live` also hands a room from one node to another under the Web, C#, C++ and C
  clients (`scripts/sdk-rehost-test.sh`). The script now exits non-zero when a client fails or the
  second node adopted no room, and `scripts/two-nodes.sh` starts the node `BIN` names.
- `ci.yml`: `sdk-live` runs the Godot check project in Godot 4.2.2 against two nodes
  (`scripts/godot-live-test.sh`): relay, sync and lockstep rooms, the slow link and a room handed
  to the second node. Until now CI built the extension and never loaded it.
- `ci.yml`: the node of the `console` job reaches Redis through a Toxiproxy service, and a browser
  spec switches it off to check what the overview, the rooms page and a project page show without
  Redis. The spec is skipped on a stand without `AURIX_NET_E2E_TOXIPROXY`; with `NET_LIVE_REQUIRED`
  set, as in this job, it fails there.
- `ci.yml`: the `console` job starts two nodes (`scripts/two-nodes.sh`), so the spec that drains
  another node runs in CI; it was skipped there. It too fails under `NET_LIVE_REQUIRED` on a stand
  without `AURIX_NET_E2E_PEER`.
- `release.yml`: the Web npm tarball is packed to the workspace root, so it sits beside the other
  SDK files in the `sdk` artifact, and the image job no longer uploads Docker build records. The
  0.1.0 run stopped at the checksum step on the `sdk/` folder; its GitHub Release was created by
  hand from the artifacts of that run.
- `release.yml`: the Web npm package and the Unity, Godot and C++ archives now carry the Apache-2.0
  licence text as `LICENSE`. The 0.1.0 SDK files were published without it.
- `ci.yml` runs on pushes to `next` as well: work lands there before it is moved to `main`.
- `release.yml`: a tag with a pre-release suffix (`v0.2.0-beta.1`) is published as a GitHub
  pre-release, and its images get the version tag only. `latest` moves on stable versions.
- `cargo test` starts a real node (`crates/net-server/tests`) and checks who may enter a room and
  what a kick, a closed room and an expired offline slot do to the other sessions: the seven node
  fixes above each have a test that fails without the fix. Needs the development Postgres and
  Redis, which the `rust` job already runs.

## [0.1.0] - 2026-11-18

One game-agnostic networking service (relay, sync and lockstep rooms in the same node) plus a
deterministic simulation toolkit, SDKs for Web/Unity/C++/Godot, a beta Unreal Engine plugin, an
operator console and a self-host deployment. Everything below is verified locally (see
[docs/testing.md](docs/testing.md)) except the Unreal plugin, which has not been built with an
engine yet.

### Added
- Bandwidth measurement for moving objects: `aurix-net-loadtest --objects N` (kB/s per client,
  objects per delta), the `movers.wat` example logic and `scripts/objects.sh`; results for 100, 250
  and 500 objects at 20 Hz in [testing.md](docs/testing.md).
- Unity gameplay framework (`Aurix.Net.Unity.Framework`, `Runtime/Framework/`): host-authoritative
  netcode over an Aurix relay room — `NetworkManager`
  (`StartHost`/`StartServer`/`StartClient`, prefab registry, `Spawn`/`Despawn`, ownership, tick),
  `NetworkIdentity` (objectId, owner, isServer/isClient/isOwner/isController, despawn on owner
  leave), `NetworkBehaviour` with `SyncVar<T>`, `SyncList<T>`, `SyncDictionary<K,V>` (server- or
  owner-authoritative, per-module send interval, onChanged events, full state on spawn/late join,
  deltas per tick) and `[ServerRpc]` / `[ObserversRpc]` / `[TargetRpc]` methods (requireOwnership,
  runLocally, bufferLast for late joiners, `RpcInfo` sender), `NetSerializer` for primitives, Unity
  math types, strings, byte[], enums, arrays/lists, `PlayerID`, `NetworkIdentity` references and
  `INetSerializable`. Reflection-bound in this version (no IL weaving); `BuildCheck~/framework-check`
  runs host + two clients through spawn, sync, RPCs, ownership transfer, late join and despawn.
- Framework components: `NetworkTransform` (quantized position/rotation/scale from the controller,
  smallest-three quaternions, tick-stamped interpolation buffer, teleport), `NetworkRigidbody`
  (velocities + extrapolation, kinematic remotes) and `NetworkAnimator` (float/int/bool parameters,
  triggers as RPCs); scene objects bound by `NetworkIdentity.sceneId` instead of instantiated;
  a `Hello`/`Welcome` handshake so a joiner gets the world only once its data path is up.
- IL weaver for the framework (`Editor/CodeGen/RpcWeaver.cs`, Mono.Cecil): `[ServerRpc]` /
  `[ObserversRpc]` / `[TargetRpc]` methods become direct call sites (`CmdHit(10)` routes the call,
  the original body moves to `CmdHit__Impl`; `[TargetRpc]` takes the target `PlayerID` as its first
  parameter). Runs in Unity through an `ILPostProcessor` (`Aurix.Net.CodeGen`, editor-only,
  not yet exercised in an editor) and on plain .NET through
  `sdk/unity/Weaver/Aurix.Net.Weaver.Cli` (`aurix-weave <assembly.dll>`), which `framework-check`
  applies to its own test assembly before running the live scenario.
- Interest management: per-object observer sets on the server, `INetworkVisibilityRule`
  (`DistanceRule`, `OwnerOnlyRule`, `AlwaysVisibleRule`, `NoVisibilityRule`, or a per-object rule),
  re-evaluated every `visibilityIntervalTicks`; spawn/despawn, deltas, ObserversRpc and TargetRpc
  follow visibility; owners and the host's player always see their objects.
- `PlayerSpawner` (one owned player object per joiner, round-robin spawn points) and
  `NetworkManager.autoStart` (Host / Server / Client / headless-server-else-client).
- Client-side prediction (`Runtime/Framework/Prediction/PredictedBehaviour<INPUT, STATE>`,
  PurrDiction-style API): the owner samples input per tick, simulates at once, keeps a history and
  sends inputs redundantly; the server simulates the owner's inputs (repeating late ones), applies
  `ServerModifyState` hooks and sends verified states; the owner reconciles on mismatch (rewind +
  replay), observers interpolate (`Interpolate`/`UpdateView`). `serverOnly` modules (inputs) are
  consumed by the server and not relayed.
- Host migration in the framework (`NetworkManager.allowHostMigration`, on by default): when the
  host leaves, the room's new master is promoted to server with the objects it holds (same ids,
  live state), other clients re-attach to it (`RelayBridge.OnPromoted` / `OnHostChanged`).
  Transport adapters keep the old behaviour (disconnect) unless `AllowHostMigration` is set.
- Editor: `NetworkManager` inspector (role, players, tick, objects with owner/observers, Start/Stop
  buttons in play mode), `NetworkIdentity` inspector (stable `sceneId` auto-assigned for scene
  objects from scene + hierarchy path, runtime owner/observers), menu "Aurix Net/Assign scene ids".
  Written against the public UnityEditor API but not yet run in an editor.
- `NetworkTransform.adaptiveSync` (Conservative/Balanced/Aggressive) with pluggable `TransformSyncStrategy` /
  `CompositeTransformSyncStrategy` (`SetSyncStrategy`): the controller skips sends while receivers can reconstruct
  the motion within tolerance (bounded by `maxSendInterval`), receivers extrapolate with the same model.
- `SyncInput<T>` (owner → server only input value with `onChanged`), `SyncTexture` (PNG over `SyncBigData`) and
  `SyncFile` (name + bytes over `SyncBigData`).
- Addressables: `AddressablesPrefabFactory` (compiled when `com.unity.addressables` is present) and the general
  `IAsyncNetworkObjectFactory`: a spawn whose prefab is still loading stays pending on the client together with
  every later message for that object (sync, ownership, RPCs, despawn), then applies in order once loaded.
- Editor: Window ▸ Aurix Net ▸ Network Statistics shows live ping/jitter/throughput/per-player pings
  (written against the public UnityEditor API, not yet run in an Editor).
- Generic RPCs: `[ObserversRpc] void Show<T>(T v)` and friends work; the weaver inserts a prologue that sends the
  call with its type arguments, the receiver closes the method (`MakeGenericMethod`) and deserialises with the
  closed parameter types. Needs the woven assembly (type arguments come from the prologue).
- RPC packing: `compression: RpcCompression.Fast|Balanced|Best` deflates argument payloads when it pays off,
  `deltaPack: true` sends only the XOR/RLE difference from the previous call of the same RPC to the same peer
  (first call to a peer always whole). Available on ServerRpc/ObserversRpc/TargetRpc. Strings up to 64 KB.
- `NetworkManager.statistics` (`NetworkStatistics`): client ping/jitter from periodic pings, per-player ping on the
  server, upload/download bytes and messages per second, high-ping/jitter and stalled-connection events.
- `ValidatedSyncVar<T>`: owner-authoritative value the server checks with `validate(current, proposed)`;
  rejected writes are reverted on the owner (`onRejected`) and never reach observers. Any module can now
  veto owner deltas via `NetworkModule.ReadDeltaFromOwner`.
- `SyncBigData`: chunked replication of large byte payloads (progress + completion events, whole payload to
  late joiners).
- `NetworkBones`: replicates local pose of SkinnedMeshRenderer bones + extra bones (per-bone change mask,
  quantised, smoothed on remotes) for ragdolls/procedural animation.
- `NetworkAudioSource`: volume/pitch/loop/mute/spatial settings as sync vars, Play/Stop/Pause/UnPause/PlayOneShot
  as RPCs, clips by index into a shared list.
- `NetworkReflection`: replicate listed fields/properties of any component or plain object by name (change-mask
  deltas, full state to late joiners) and call its methods over the network via `Invoke(name, args)` with a
  Server/Observers/Target type per method.
- `StateMachine` + `StateNode`/`StateNode<T>`: replicated state machine (server- or owner-authoritative);
  `SetState`, `SetState(node, data)`, `Next`, `Stop`; Enter/Exit/StateUpdate on every peer with the same
  payload, late joiners enter the current state.
- `NetworkLOD`: per-player send-rate tiers by distance (interval per tier, hysteresis, cull beyond the last
  tier); gates module deltas only, visibility/RPCs untouched; `onTierChanged`.
- `StateMachine` + `StateNode`/`StateNode<T>`: replicated state machine (server- or owner-authoritative);
  `SetState`, `SetState(node, data)`, `Next`, `Stop`; Enter/Exit/StateUpdate on every peer with the same
  payload, late joiners enter the current state.
- `NetworkLOD`: per-player send-rate tiers by distance (interval per tier, hysteresis, cull beyond the last
  tier); gates module deltas only, visibility/RPCs untouched; `onTierChanged`.
- `NetworkTransform.syncParent` + `SetParent(NetworkIdentity)`: the parent object id travels with the
  pose, remotes re-parent and apply the pose in the parent's local space while attached.
- Static RPCs: `[ServerRpc]`/`[ObserversRpc]`/`[TargetRpc]` on static methods of any class, routed via
  `NetworkManager.main` (the active manager; `SetMain` to pick one); identified on the wire by an FNV-1a
  hash of type/method/arg-count so the weaver and runtime agree across assemblies; `bufferLast` replays
  to late joiners. Weaver handles them (`__StaticRpcSend`, `__StaticLocalInfo`).
- Lag compensation: `ColliderRollback` keeps a per-tick pose history on the server; clients report
  their view tick (`viewDelayTicks` behind the newest server tick, `Msg.ViewTick`) and the server
  broadcasts its clock every tick; `NetworkManager.Rollback(shooter, body)` rewinds all rollback
  components to the shooter's view tick around `body` and restores them.
- Sync modules: `SyncHashSet<T>`, `SyncQueue<T>`, `SyncArray<T>` (changed slots only), `SyncEvent<T>`
  (fire-and-forget payload, not replayed to late joiners) and `SyncTimer` (server-tick countdown,
  `remaining`/`finished`/`Poll()`), same authority/full/delta rules as `SyncList`.
- Quick match: `NetworkManager.StartAuto()` joins a random open room matching `matchFilterJson` /
  `matchExpr` as a client, or hosts a fresh room with `roomPropsJson` when none exists
  (`RelayBridge.StartAuto()`; the same settings are available to the transport adapters).
- Unity: transports for third-party netcode over an Aurix relay room — `AurixNetTransport`
  (PurrNet `GenericTransport`/`ITransport`), `AurixNetMirrorTransport` (Mirror `Transport`) and
  `AurixNetFishNetTransport` (FishNet `Transport`) on a shared `RelayBridge`: the host is the room
  master, host→client frames are targeted events, client→host frames go to the master, connection
  ids are actor ids, reliable channels ride the reliable relay channel (fragmented above the
  datagram size), kicks map to the framework's disconnect. Compiled only when the framework is
  present (`AURIX_PURRNET` version define for `dev.purrnet.purrnet`, Mirror's `MIRROR`, FishNet's
  `FISHNET`). No host migration yet. `BuildCheck~/transports-check` runs host + two clients through
  each adapter against a local node.
- `get_rooms{expr?, limit?}`: one-shot filtered room list on the control channel (SQL-lobby
  style filter); Web `client.rooms()`, C# `RoomsAsync`.
- WebRPC: `rpc{id, name, body}` on the control channel calls the project's webhook subscribed to
  `rpc` (signed POST, 5 s, ≤ 16 KiB answer) and returns `rpc_result`; Web `client.rpc()`, C#
  `RpcAsync`, Rust `Client::rpc`. 5 calls/s per session, body ≤ 8 KiB.
- Sync (owner-authoritative rooms): clients can `spawn`/`write`/`despawn`/`transfer` entities they
  own (op list input, ≤ 32 spawned per actor); a leaving actor's entities are despawned. Web
  `SyncClient.spawn/despawn/transfer/writeEntity`, C# `SyncWorld.EncodeOps`/`SyncOp`, C ABI
  `aurix_net_sync_send_op` (C++ `Sync::spawn/despawn/transfer/writeEntity`).
- Unreal Engine plugin (`sdk/unreal`), beta: `AurixNetDriver` runs the engine's own replication and
  RPCs over a relay room, `UAurixNetSubsystem` and async nodes for Blueprint, an Online Subsystem
  for the standard session nodes, sync and lockstep components, physics prediction, aurix-sim with
  a rollback runner. Written for UE 5.3-5.8 and not yet built with an engine: CI runs the
  engine-free checks only (`sdk/unreal/scripts/check_plugin.py`).

### Fixed
- Unity transport adapters: `AurixNetMirrorTransport` clients are no longer kicked on connect (the
  server-side connect callback was raised twice and a repeated connection id is rejected);
  `AurixNetMirrorTransport` and `AurixNetFishNetTransport` pass on the channel a frame arrived on
  instead of always "reliable"; host mode works in `AurixNetTransport` and
  `AurixNetFishNetTransport` (the local client is linked to the host in-process instead of joining
  the room a second time under the same grant, which replaced the host's session); a connect that
  is stopped before it completes no longer leaves a player in the room;
  `AurixNetMirrorTransport.ServerUri()` keeps the room name intact (`aurix://room/<name>`).
  `transports-check` covers all of these.
- Re-hosting: two players rejoining the same orphaned room through one node at the same time could
  end up in two rooms of the same name (the row was mistaken for a crash leftover while the
  adoption was still in flight). The node now tracks in-flight adoptions, concurrent joins get
  `room_pending` and retry for up to 2 s; `join` retries too. Found on a two-host run
  ([docs/testing.md](docs/testing.md), Multi-host).
- `chrono` is unmaintained (RUSTSEC-2026-0543) and is replaced by `time`. Dates in REST
  responses, CSV exports and webhook payloads are written exactly as before.

### CI
- `release.yml`: tag-driven release (version/CHANGELOG check, server bundles for linux
  x86_64/aarch64, SDK archives, SBOM, multi-arch GHCR images, GitHub Release with checksums).
  `soak.yml`: nightly 10-minute two-node soak with RSS/overrun verdict and the CSV attached.
- Docker job loads the built images into the daemon before the Trivy scan; the Godot
  GDExtension step builds `libaurix_net` first.

### Node (`aurix-net-server`)
- Transport: QUIC datagrams natively, WebTransport (HTTP/3) with a WebSocket binary fallback in
  browsers; reliable/unreliable/reliable-unordered channels, fragmentation (64 KiB), coalescing,
  per-datagram batching; control plane over WebSocket JSON and REST (Axum).
- Relay rooms: events with target groups, interest groups, event cache (room/slot/slices),
  room and player props with TTL, master client, slot takeover, spectators, kick, lobby
  statistics (`stats`), friend presence (`find_friends`), webhooks with HMAC + SSRF guards.
- Sync rooms: server-authoritative `Frame` (entities, schema components, bit codec, deltas,
  acks, lag-compensation history), owner-authoritative writes, AOI grid with priority budgets and
  `set_view` viewpoints/interest, simulation events, optional server-side WASM room logic.
- Lockstep rooms: server clock, redundant input bundles, canonical journal (Redis), late join
  with checkpoints, desync detection with reference dumps, predict/rollback runner, and a
  **referee**: the node runs the room's WASM simulation itself (Wasmtime, fuel/memory/log budgets)
  and its state is the truth (`referee: true`, actor `0` dump).
- Matchmaking: Redis-backed tickets shared across nodes, leader-locked cycle, attrs + SQL-like
  filters, bot backfill, `fill_existing`.
- Multi-node: Postgres + Redis, per-node heartbeat liveness, drain/handoff on `SIGTERM`,
  rolling upgrades, rehost/rejoin, regions as a first-class field, cross-node operator calls.
- Security: API keys (SHA-256, scopes, expiry, CIDR allow-lists), short-lived player grants with
  secret rotation (`grant_secret_previous`), named operator tokens, audit log of every mutation,
  quotas and monthly caps per project, bounded decoders fuzz-tested (`tests/garbage.rs`), JSON
  depth/size limits, `/metrics` behind a token.
- Operations: `--migrate-only`, `--healthcheck`, Prometheus alert rules, backup script
  (pg_dump + Redis RDB + SHA-256 manifest), distroless non-root image (62.6 MB), hardened compose
  (read-only rootfs, `cap_drop: [ALL]`, `no-new-privileges`), Caddy TLS example, SBOM + Trivy +
  `cargo deny` in CI, mimalloc allocator.

### aurix-sim (deterministic, Q32.32 fixed point, native == wasm32)
- `net-fpmath` (vec2/vec3/quat/mat3/RNG), `net-sim` (ECS frame, snapshot/restore/hash).
- `net-sim-physics` 2D: circles/boxes/polygons/capsules, joints (distance/revolute/weld),
  character controller, ray/circle casts, contact events.
- `net-sim-physics3d`: spheres/capsules/boxes/convex hulls (SAT + face clipping), ball/hinge/
  distance joints, kinematic capsule character controller, ray/sphere/capsule/box sweeps.
- `net-sim-nav`: navmesh import, A* + funnel, raycast on mesh, flow fields, crowd steering.
- `net-sim-bots`: behavior trees + HFSM + blackboard (Rust, with TS and C# ports sharing JSON vectors).
- One flat ABI (`net-sim-wasm`, 98 functions) for Web/Unity (wasm/native) and C/C++/Godot
  (`aurix_sim.h`); six golden hashes checked native == wasm32 in CI.

### SDKs
- Web (`@aurix/net`): relay/sync/lockstep clients, rollback runner, matchmaking, presence,
  Sim wrappers; 28 tests incl. live ones.
- Unity (UPM): P/Invoke client, `NetRunner`/`NetView`/`SyncBehaviour`, `NetTransform`,
  `NetRigidbody`, `NetAnimator`, `TickTimer`, spawn hooks, lockstep client + rollback, Sim wrappers;
  managed tests under dotnet (editor run pending a licence).
- Rust (`net-client`), CLI (`aurix-net`), C ABI (`libaurix_net` + `aurix_net.h`/`aurix_sim.h`),
  C++ header-only wrapper, Godot 4 GDExtension (relay + sync, verified headless).
- Operator console (React/Vite SPA behind Caddy): projects, keys, quotas, rooms, nodes, desync
  dumps, matchmaking queue, usage CSV, attention panel; Playwright suite against a live node.

### Not in scope
- Client-hosted P2P topologies, coupling with Aurix Voice (a separate service a game uses
  independently).
