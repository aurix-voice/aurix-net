# Aurix Net for Unreal — quick start

The plugin gives Unreal two things:

1. **`AurixNetDriver`** — an Unreal NetDriver over an Aurix relay room. Replication, RPCs,
   `PlayerController` login, everything the engine does stays the same; packets travel through the
   Aurix cell as relay events, so a listen server needs no public IP or port forwarding.
2. **`UAurixNetSubsystem`** — Blueprint access to the room: connect with a grant, join or create,
   send and receive your own relay events, see players come and go.

## 1. Native library

```sh
sdk/unreal/scripts/build_native.sh                 # host platform → Source/ThirdParty/AurixNetLibrary/lib/<Platform>
sdk/unreal/scripts/build_native.sh --target x86_64-pc-windows-msvc
```

Release zips on GitHub ship the plugin with the natives already staged.

## 2. Project settings

`Config/DefaultEngine.ini` of your game:

```ini
[/Script/Engine.Engine]
!NetDriverDefinitions=ClearArray
+NetDriverDefinitions=(DefName="GameNetDriver",DriverClassName="/Script/AurixNet.AurixNetDriver",DriverClassNameFallback="/Script/OnlineSubsystemUtils.IpNetDriver")
```

The driver's timeouts and rates ship in the plugin's `Config/Engine.ini`. To change one, repeat
its line under `[/Script/AurixNet.AurixNetDriver]` in your `DefaultEngine.ini`.

## 3. Connect, then travel

Your backend mints a player grant with the SDK key (`POST /v1/grants` on the cell); while
prototyping use a **development grant** from the console (Networking → Keys & grants).

Blueprint (or C++) once per game instance:

```
Get Game Instance → Get Subsystem (Aurix Net) → Connect (Cell Url, Grant)
```

Host: `Open Level` with a map and `?listen?Room=my-room`, for example
`/Game/Maps/Arena?listen?Room=my-room` (joins or creates the relay room and becomes the master /
listen server). Clients: `Open Level` with `my-room.aurix`. A dedicated server works the same way
with `?listen`; it is just a player that only hosts.

The engine listens only on a local map URL and reads a host only from a name with a dot, so the room
is the `Room=` option for the host and the `<room>.aurix` host name for a client (`?Room=` works for
a client too, for room names that are not valid host names). `aurix://<room>` is not a URL the
engine accepts. `MaxPlayers=`, `Visible=0`, `Open=0`, `Props=<JSON object>` and
`LobbyProps=<JSON array>` of the room the host creates come from the URL. The lobby props are the
names of the props that room listings show and filters match, their values are in the props:
`PropsJson` `{"map":"dust","rank":3}` with `LobbyPropsJson` `["map","rank"]`; `Join Or Create Ex`
takes the names as a list of strings instead. Matchmaking and the room list are on the subsystem
(`Join Random`, `Request Rooms`, `Join Or Create Ex` with the player ids whose seats the room
holds).

`OnLeft(Reason, RoomId)` fires when the cell takes the player out of a room: a kick, the room
closing, the same player joining from another connection, or `rehost`, when the room moved to
another node; on `rehost` connect again with the same grant and join the same room.

## 3b. Async nodes, settings, PIE

**Project Settings → Plugins → Aurix Net**: cell URL, tunnel, default max players, host migration,
interest cell size. **Aurix Net (Editor)** (per user, never cooked): a development SDK key — the
**Connect with Dev Grant (PIE)** node then mints one player per PIE instance, so "Number of Players = 3"
in the editor gives three distinct players in one room without a backend.

Latent nodes (Success / Failure pins, nothing blocks the game thread): **Connect to Aurix**,
**Connect and Join Room**, **Join or Create Room**, **Join Room**, **Join Random Room**.

Realtime-style room features on the subsystem: `SetRoomProps` / `SetPlayerProps` (with CAS
`Expected`), `SetRoomVisibility`, `SubscribeLobby` (+ `OnLobbyUpdate`; a room that left the list
comes with its `RoomId` alone, the name and the rest empty, so take it out by `RoomId`),
`SendEventEx` (interest group, cached events for late joiners), `SetGroups`, `Rpc` to a room module,
`Kick`, `SetMaster`, `RequestStats`, `OnDesync`. `AurixInterestComponent` joins grid interest groups
around its owner — send state events with `Group = CurrentGroup()` and only nearby players receive
them.

## 3c. Host migration

With **Host migration** on (default), a relay room survives its host leaving: the cell promotes a
new master, that player reloads the current map as the listen server on the same room, the others
reconnect to it about a second later. Game state is yours to carry: on `OnHostMigration(Starting)`
save what matters (room props are kept by the cell — `SetRoomProps`), on `OnHostMigration(Done,
bNewHost = true)` restore it in the new host's world. Turn the setting off to get the classic
"host left, everyone disconnects" behaviour.

## 4. Standard session nodes (optional)

Enable the Aurix Online Subsystem to use **Create Session / Find Sessions / Join Session** as usual:

```ini
[OnlineSubsystem]
DefaultPlatformService=Aurix
```

Create Session creates (or joins as host) a relay room named after the session — set the
`AURIXROOM` extra setting to pick the room name — Find Sessions lists open relay rooms of the
cell, Join Session joins one; the resolved connect string is `<room>.aurix`, which the
AurixNetDriver understands, so `Client Travel` after Join works unchanged. Friends, invites
and presence are not implemented (return false).

The player of the sessions is the player the grant names. While the plugin is connected the
unique net id of local user 0 is the player's id on the cell (`GetPlayerId` of the subsystem)
and the nickname is the grant's name; before that there is no id and the login status is
`NotLoggedIn`. The plugin writes the id to the local player and its player state when the
connection is made and takes it away when it is lost, so the Create Session node finds it
whether the game connected before or after the map loaded. Login and AutoLogin succeed once
connected; in the editor Login connects with a development grant first, and AutoLogin, which the
engine calls itself when a networked game starts, never connects. Logout disconnects.

Session settings and search params reach the room:

- A setting advertised via the online service (`ViaOnlineService`, `ViaOnlineServiceAndPing`)
  becomes a room prop the room publishes, named by its key in lower case; strings, numbers and
  booleans are carried. Find Sessions returns them in the settings of each result. An
  integer setting comes back as `int32` when its value fits and as `uint64` above that; a
  `float` or `double` setting comes back as `double`. The settings of the engine take no
  signed 64-bit integer, so an integer below the `int32` range comes back as `double`, exact
  down to -2^53.
- `bShouldAdvertise` is the room's visibility. Start Session closes the room unless
  `bAllowJoinInProgress` is set, End Session opens it again. Closed rooms are not listed.
- Update Session with the refresh flag sends the advertised values to the room. Which props a
  room publishes is fixed when it is created.
- Search params with the comparisons equals, not equals, greater and less (with or without
  equals) become the room filter of Find Sessions and of the random join of matchmaking.
  `SEARCH_MINSLOTSAVAILABLE` is applied to the results; the other search options of the engine
  are ignored. A param the filter cannot express (`Near`, `In`, `NotIn`, a name outside
  letters, digits, `_` and `.`) fails the search, with the reason in the log.
- Seats are held for the player ids listed, comma separated, in the `AURIXRESERVED` setting,
  and for the users passed to Start Matchmaking after the first, when this session creates the
  room. The ids are the players' ids on the cell.
- The cell answers one room list a second, so Find Sessions called sooner fails.

## 5. Sync and lockstep rooms

Relay is what the NetDriver uses. The other two room modes are exposed as components on any actor:

- **`AurixSyncWorld`** — client of a cell-authoritative world (`sync` rooms): set the schema JSON,
  read entities/components/fields each tick (`OnSpawn` / `OnChange` / `OnDespawn`), send inputs for the
  room's server logic, or owner-authoritative writes (`WriteBegin`, `WriteVector`…, `WriteEnd`, then
  `SendWrites`).
- **`AurixLockstepComponent`** — deterministic lockstep (`lockstep` rooms): `SendInput` each tick,
  simulate on `OnTick(Tick, Inputs)`, `ReportHash` so the cell catches desyncs; `OnRestore` and
  `OnStateRequest` handle late join / referee checkpoints.

`SendCommand` sends a reliable command outside the input stream (buy, surrender, pause); the cell stamps it onto the next tick and everyone receives it through `OnCommand` right before `OnTick` of that tick, so simulations stay in step. With `AurixRollbackRunner` take the commands from `OnSimulate` instead (section 6). Commands are journaled and part of replays. A command is 1 to 251 bytes; a longer one is refused (`SendCommand` returns false, the reason is in the subsystem's `GetLastError`).

**Shared mode without codegen** - `AurixSyncActorComponent` on any actor: pick the schema component
and the UPROPERTY names (same as the field names); the owner writes changed values into the sync room
(`WriteHz`), everyone else gets them applied to the same properties. `bSyncTransform` maps the root
component to `position` / `rotation` fields. `Spawn` (or `bSpawnOnBeginPlay`) creates an entity you own,
`BindEntity` attaches a remote one (from `AurixSyncWorld.OnSpawn`), `TransferTo` hands ownership over;
late joiners get the cell's snapshot. `OwnershipPolicy` picks what the cell does when the owner leaves (destroy / to master / orphan / dynamic) and who may `RequestOwnership` (`OnOwnershipDenied` when refused); `Release` drops ownership. Types: bool, ints/enums, float/double, FVector/FVector2D,
FRotator/FQuat, FString/FName, TArray<uint8>.

Both read the room's data stream from the game instance's subsystem; join the room with the matching
mode first (`Join Or Create` with Mode = Sync / Lockstep).

## 5b. Physics prediction

`AurixPhysicsPredictionComponent` on a physics actor: **EngineReplication** picks Chaos predictive
interpolation (5.4+) or resimulation (5.5+) for bodies replicated over the NetDriver; **RelayEvents**
is the NetDriver-less path — the owner streams snapshots (event 201, optional interest group), the
others extrapolate with the received velocities and blend corrections over `CorrectionTime`.

Console: `aurix.net.status`, `aurix.net.rooms [expr]`, `aurix.net.leave`. In the editor, **Window → Developer Tools → Aurix Net Debugger** shows every PIE instance: state, room, players, props, lockstep statistics, with Request rooms / Leave / Project Settings buttons.

## 6. Deterministic simulation and rollback

- **`AurixSimWorld2D` / `AurixSimWorld3D`** — aurix-sim in Blueprint/C++: fixed-point (Q32.32) rigid
  bodies, joints, raycasts and sweeps, kinematic character controllers, grid navmesh + flow fields +
  crowd (2D). Bit-identical on every machine and on the cell's WASM referee: `Hash`, `Snapshot`,
  `Restore` are exact. Doubles at the API edge, integers inside.
- **`AurixRollbackRunner`** — predict-and-rollback over `AurixLockstepComponent`: `SetLocalInput` each
  frame, it sends the input, simulates predicted ticks at `TickHz`, and on every confirmed tick
  restores the confirmed state, applies the confirmed inputs, re-simulates the predicted ticks and
  reports the hash (the cell catches desyncs). Bind `OnSimulate` (your deterministic step),
  `SaveState` / `LoadState` (e.g. `Snapshot` / `Restore` of a sim world). `OnSimulate(Tick, Inputs,
  Commands, bPredicted)` hands over the commands of a confirmed tick (`SendCommand`): run them
  before the step. A predicted tick has none, so with the runner do not apply commands from the
  lockstep component's `OnCommand`. Late joiners receive a checkpoint through `OnRestore`;
  `ExportReplay` / `PlayReplay` give exact replays from inputs and commands.

## Engine versions

Beta: built and run with UE 5.8.2 on Linux only. The range below is what the source is written
for; the other engine versions and platforms have not been built.

5.3 – 5.8; Win64, Linux, Mac, Android, iOS (static `libaurix_net.a`). One source tree.
Package per engine with `RunUAT BuildPlugin -Plugin=…/AurixNet.uplugin -Package=…`.

## Dedicated server with a public IP

Run a node on the game-server machine (`aurix-net-agent install` from the console, or the self-hosted compose) and set **Project Settings → Aurix Net → Dedicated Node Url** to `http://127.0.0.1:8090`. `-server` builds then create their room through that node; players keep `CellUrl` and are redirected straight to your machine. See `docs/modes.md` → Topologies.
