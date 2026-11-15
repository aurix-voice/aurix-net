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

[/Script/AurixNet.AurixNetDriver]
NetConnectionClassName="/Script/AurixNet.AurixNetConnection"
```

## 3. Connect, then travel

Your backend mints a player grant with the SDK key (`POST /v1/grants` on the cell); while
prototyping use a **development grant** from the console (Networking → Keys & grants).

Blueprint (or C++) once per game instance:

```
Get Game Instance → Get Subsystem (Aurix Net) → Connect (Cell Url, Grant)
```

Host: `Open Level` with `aurix://my-room?listen` (joins or creates the relay room and becomes the
master / listen server). Clients: `Open Level` with `aurix://my-room`. A dedicated server works the
same way with `?listen`; it is just a player that only hosts.

Room name, `MaxPlayers=` and props come from the URL; matchmaking and the room list are on the
subsystem (`Join Random`, `Send Control` with lobby requests from docs/api.md).

## 3b. Async nodes, settings, PIE

**Project Settings → Plugins → Aurix Net**: cell URL, tunnel, default max players, host migration,
interest cell size. **Aurix Net (Editor)** (per user, never cooked): a development SDK key — the
**Connect with Dev Grant (PIE)** node then mints one player per PIE instance, so "Number of Players = 3"
in the editor gives three distinct players in one room without a backend.

Latent nodes (Success / Failure pins, nothing blocks the game thread): **Connect to Aurix**,
**Connect and Join Room**, **Join or Create Room**, **Join Room**, **Join Random Room**.

Realtime-style room features on the subsystem: `SetRoomProps` / `SetPlayerProps` (with CAS
`Expected`), `SetRoomVisibility`, `SubscribeLobby` (+ `OnLobbyUpdate`), `SendEventEx` (interest
group, cached events for late joiners), `SetGroups`, `Rpc` to a room module, `Kick`, `SetMaster`,
`RequestStats`, `OnDesync`. `AurixInterestComponent` joins grid interest groups around its owner —
send state events with `Group = CurrentGroup()` and only nearby players receive them.

## 3c. Host migration

With **Host migration** on (default), a relay room survives its host leaving: the cell promotes a
new master, that player reloads the current map as the listen server on the same room, the others
reconnect to it about a second later. Game state is yours to carry: on `OnHostMigration(Starting)`
save what matters (room props are kept by the cell — `SetRoomProps`), on `OnHostMigration(Done,
bNewHost = true)` restore it in the new host's world. Turn the setting off to get the classic
"host left → everyone disconnects" behaviour.

## 4. Standard session nodes (optional)

Enable the Aurix Online Subsystem to use **Create Session / Find Sessions / Join Session** as usual:

```ini
[OnlineSubsystem]
DefaultPlatformService=Aurix
```

Create Session creates (or joins as host) a relay room named after the session — set the
`AURIXROOM` extra setting to pick the room name — Find Sessions lists open relay rooms of the
cell, Join Session joins one; the resolved connect string is `aurix://<room>`, which the
AurixNetDriver understands, so `Client Travel` after Join works unchanged. Friends, invites
and presence are not implemented (return false).

## 5. Sync and lockstep rooms

Relay is what the NetDriver uses. The other two room modes are exposed as components on any actor:

- **`AurixSyncWorld`** — client of a cell-authoritative world (`sync` rooms): set the schema JSON,
  read entities/components/fields each tick (`OnSpawn` / `OnChange` / `OnDespawn`), send inputs for the
  room's server logic, or owner-authoritative writes (`WriteBegin` → `WriteVector`… → `WriteEnd` →
  `SendWrites`).
- **`AurixLockstepComponent`** — deterministic lockstep (`lockstep` rooms): `SendInput` each tick,
  simulate on `OnTick(Tick, Inputs)`, `ReportHash` so the cell catches desyncs; `OnRestore` and
  `OnStateRequest` handle late join / referee checkpoints.

Both read the room's data stream from the game instance's subsystem; join the room with the matching
mode first (`Join Or Create` with Mode = Sync / Lockstep).

## Engine versions

5.3 – 5.8. One source tree; the module compiles with `CppStandardVersion.Default` (5.6+ needs
C++20). Package per engine with `RunUAT BuildPlugin -Plugin=…/AurixNet.uplugin -Package=…`.
