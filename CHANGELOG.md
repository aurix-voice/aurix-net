# Changelog

All notable changes to Aurix Net. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow SemVer. Nodes of one deployment run the same minor version; the `aurix-net:<tag>`
image, the `sbom` artifact and this file's entry are produced from the tagged commit
([release checklist](docs/release-checklist.md)).

## [Unreleased]

### Changed
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

### Fixed
- Unreal plugin: first build and run with an engine (UE 5.8.2, Linux). `RunUAT BuildPlugin` passes
  for the editor and the game (Development, Shipping), the packaged plugin loads, and a listen
  server with two clients goes through login, replication and host migration. Before this the
  plugin did not compile (13 errors) and could not connect once it did:
  - travel URLs: `aurix://<room>` is not a URL the engine accepts. A host now opens
    `<map>?listen?Room=<room>` and a client opens `<room>.aurix`; session connect strings and the
    host-migration travel use the same forms;
  - the listen server runs the engine's stateless handshake before it creates a connection;
  - a connecting client finds the subsystem through its world context (its driver has no world yet);
  - the packet size follows the engine cap (`MAX_PACKET_SIZE`), and the driver sets the timeouts the
    engine ships only for its own drivers;
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
- SDK docs: the Godot and C++ lockstep examples labelled a checkpoint with the tick from the
  request, so a late joiner desynced; they now use the simulation's own tick. The Godot README no
  longer points to a `rejoin()` that does not exist, and the event codes taken by sync and lockstep
  rooms are stated as they are (`0x10–0x2F`, in those rooms only).
- Godot SDK: the `event` signal no longer fires for sync and lockstep frames in rooms of those
  modes. Only relay events reach it, as in the Web SDK.
- Node: `create` on the control channel holds the seats listed in `reserved`, as REST room
  creation and `join_or_create` do. The field was ignored there.

### CI
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
