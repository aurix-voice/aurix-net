# Changelog

All notable changes to Aurix Net. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow SemVer. Nodes of one deployment run the same minor version; the `aurix-net:<tag>`
image, the `sbom` artifact and this file's entry are produced from the tagged commit
([release checklist](docs/release-checklist.md)).

## [Unreleased]

### Added
- Unity gameplay framework (`Aurix.Net.Unity.Framework`, `Runtime/Framework/`): host-authoritative
  netcode in the PurrNet/Mirror/FishNet mould over an Aurix relay room — `NetworkManager`
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
  parameter). Runs in Unity through an `ILPostProcessor` (`Aurix.Net.CodeGen`, editor-only, same
  layout as Netcode for GameObjects — not yet exercised in an editor) and on plain .NET through
  `sdk/unity/Weaver/Aurix.Net.Weaver.Cli` (`aurix-weave <assembly.dll>`), which `framework-check`
  applies to its own test assembly before running the live scenario.
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

### Fixed
- Re-hosting: two players rejoining the same orphaned room through one node at the same time could
  end up in two rooms of the same name (the row was mistaken for a crash leftover while the
  adoption was still in flight). The node now tracks in-flight adoptions, concurrent joins get
  `room_pending` and retry for up to 2 s; `join` retries too. Found on a two-host run
  ([docs/testing.md](docs/testing.md), Multi-host).

### CI
- `release.yml`: tag-driven release (version/CHANGELOG check, server bundles for linux
  x86_64/aarch64, SDK archives, SBOM, multi-arch GHCR images, GitHub Release with checksums).
  `soak.yml`: nightly 10-minute two-node soak with RSS/overrun verdict and the CSV attached.
- Docker job loads the built images into the daemon before the Trivy scan; the Godot
  GDExtension step builds `libaurix_net` first.


## [0.1.0] — first release candidate

One game-agnostic networking service — relay, sync and lockstep rooms in the same node — plus a
deterministic simulation toolkit, SDKs for Web/Unity/C++/Godot, an operator console and a
self-host deployment. Everything below is verified locally (see [docs/testing.md](docs/testing.md)).

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
- Unreal SDK (the engine ships its own netcode), client-hosted P2P topologies, tight coupling with
  Aurix Voice (separate service), managed cloud (self-host first).
