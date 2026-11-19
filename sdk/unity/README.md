# Aurix Net for Unity

Unity 2022.3 or newer. Pure C#, no native library, every Unity platform except Web. A Web build
cannot run the client yet: it talks through `System.Net.WebSockets` and `System.Net.Http` and
runs its reader and timer on threads, and Unity has none of the three in a browser. Install as a
UPM package (`Packages/manifest.json` → `"com.aurix.net": "file:../../sdk/unity"` or a git URL).
JSON is read with Newtonsoft.Json; the package pulls in Unity's `com.unity.nuget.newtonsoft-json`.

## Quick start (sync mode, ~10 minutes)

1. **Window > Aurix Net** → *Create settings*. Fill `ServerUrl` and, for the editor only,
   `DevApiKey` (production builds get grants from your backend via `POST /v1/grants`).
2. Add a **NetRunner** to the scene, assign the settings, set `AutoJoinRoom` (e.g. `arena-1`).
3. Make a player prefab: **NetView** + **NetTransform** (+ mesh). Drop it into `Settings.Prefabs`.
4. Press Play in two editors / builds: each client gets its own avatar, moves it, the other sees
   it interpolated.

Replicate your own state with `[Sync]` fields on a `SyncBehaviour`; they map onto a component
of the schema in the settings (component name = `[SyncComponent("Name")]` or the class name):

```text
component Transform { pos: vec3 @range(-1024, 1024, 0.01)  rot: quat }   // used by NetTransform
component Player    { name: string(16)  hp: u8 }
```

```csharp
[SyncComponent("Player")]
public class PlayerController : SyncBehaviour
{
    [Sync("name")] public string PlayerName;
    [Sync] public byte hp = 100;                        // field name = schema field name

    void Update()
    {
        if (!IsMine) return;                            // only the owner moves its avatar
        transform.position += Speed * Time.deltaTime * new Vector3(Input.GetAxis("Horizontal"), 0, Input.GetAxis("Vertical"));
        if (Input.GetKeyDown(KeyCode.Space)) View.Rpc(nameof(Wave), transform.position);
    }

    [Rpc] void Wave(Vector3 at) => Debug.Log($"{PlayerName} waves at {at}");
    protected override void OnSynced(uint tick) { /* remote copy updated from a server delta */ }
}
```

The owner's `[Sync]` values go to the server as its input every tick; the server (owner-authoritative
by default, or your WASM logic with `props.logic`) replicates deltas to everyone in range (AOI),
`NetTransform` interpolates `InterpolationTicks` behind the server. `[Rpc]` methods travel as
reliable relay events (`bool, int, long, float, string, byte[], Vector3, Quaternion` args).

Prediction (server-authoritative movement with `props.logic`): keep the local player responsive without
waiting a round trip. Send raw inputs with `Runner.SendInput(bytes)` (returns the tick), apply the same
step locally through `Predictor<TState>` (`Aurix.Net.Managed`), and reconcile when your own entity's
server state arrives:

```csharp
var pred = new Predictor<Vector3>(transform.position, (pos, input) => Move(pos, input), (a, b) => (a - b).sqrMagnitude > 0.0001f);
pred.OnCorrected += (was, now) => _visualError += was - now;   // decay _visualError over a few frames
View.Runner.OnOwnEntityUpdated += (view, entity, tick) => { if (view == View) pred.Reconcile(tick, ReadPos(entity)); };
void Tick(byte[] input) { uint t = View.Runner.SendInput(input); transform.position = pred.Record(t, input) + _visualError; }
```

`Move` must be the rule your WASM logic applies for one tick of that input (same fixed-point/float math); a
mismatch is corrected by replaying the unconfirmed inputs on the server state (`Mispredictions` counts them).
Owner-authoritative rooms (no `logic`) do not need prediction: the owner's `[Sync]` values are the truth.

Relay rooms (no simulation): `Runner.SendEvent(code, bytes, Target.All, reliable: true)` and
`Runner.OnEvent`. An unreliable event that arrives after a newer one is dropped;
`Runner.SendEventOn(Channel.UnreliableUnsequenced, code, bytes)` sends one that still arrives. In sync and lockstep rooms event codes `0x10–0x2F` belong to the room's own frames:
an event sent with one of them is not delivered.

Lockstep rooms (deterministic, every client simulates): `Runner.Lockstep` delivers confirmed inputs
per tick in order (`OnTick`), restores state on late join / rehost (`OnRestore`), uploads the master's
checkpoint when the node asks (`OnCheckpointRequested` → `SendCheckpoint`); you send `SendInput(bytes)`
every tick (it returns the tick the input is stamped with; the room takes one input per tick, extra calls send nothing and return -1) and `ReportHash(tick, hash)` so the node can flag desyncs. Keep the simulation integer or
fixed-point; see `Samples~/LockstepDuel/DuelSim.cs`.

Samples (`Samples~`): `SyncArena/PlayerController.cs`, `RelayChat/ChatSample.cs`, `LockstepDuel/DuelSim.cs`.

## Layers

| Layer | Namespace | What |
|---|---|---|
| Unity | `Aurix.Net.Unity` | `NetSettings`, `NetRunner`, `NetView`, `NetTransform`, `SyncBehaviour` + `[Sync]`/`[Rpc]`, editor window |
| Managed core | `Aurix.Net.Managed` | `ManagedClient` (WebSocket control + tunnel data, same packets/channels/fragmentation as QUIC), `Schema` (IDL parser), `SyncWorld` (delta decode, acks, owner inputs), bit codec |
| Native (optional) | `Aurix.Net.Client` | P/Invoke into `libaurix_net` for QUIC datagrams; needs the binary per platform, not WebGL |

Lower level, no scene objects:

```csharp
var c = await ManagedClient.ConnectAsync("https://net.example.com", grant);
c.OnEvent += f => Debug.Log($"event {f.Code} from {f.From}");
await c.JoinOrCreateAsync("lobby-1", "relay", 8);
c.SendEvent(1, Encoding.UTF8.GetBytes("hi"), Target.Others, reliable: true);
void Update() { c.Poll(); }  void LateUpdate() { c.Flush(); }
```

`Poll` raises the events on the thread that calls it. The link itself does not wait for it: the
client reads, acknowledges and resends off the game thread, so a frame that takes seconds does
not cost the room.

## Gameplay framework (host-authoritative)

`Runtime/Framework/` is a complete netcode layer for games that want a Unity host (or a headless
Unity server) to own the simulation, with Aurix Net as the relay, lobby and hosting underneath:

```csharp
using Aurix.Net.Unity.Framework;

public class Player : NetworkBehaviour
{
    public SyncVar<int> health = new SyncVar<int>(100);                        // server writes
    public SyncVar<Vector3> aim = new SyncVar<Vector3>(default, 0.05f, ownerAuth: true); // owner writes, 20/s
    public SyncList<int> inventory = new SyncList<int>(ownerAuth: true);
    public SyncDictionary<string, int> stats = new SyncDictionary<string, int>();

    [ServerRpc] void CmdHit(int dmg, RpcInfo info = default) { health.value -= dmg; } // owner → server
    [ObserversRpc(bufferLast: true)] void RpcShout(string s) { ... }         // server → everyone (late joiners get the last one)
    [TargetRpc] void RpcWhisper(string s) { ... }                            // server → one player

    void Update() { if (isOwner && Input.GetMouseButtonDown(0)) CmdHit(10); }        // woven: a plain call
    protected override void OnSpawned(bool asServer) { health.onChanged += h => bar.value = h; }
}
```

`NetworkManager` (one per scene): `httpUrl`, `grant`/`GrantProvider`, `roomName`, `maxPlayers`,
`tickRate`, `networkPrefabs`; `StartHost()` (server + local player), `StartServer()` (headless),
`StartClient()`, `Stop()`; `Spawn(prefab, owner?)`, `Despawn(id)`, `SetOwnership(id, player?)`;
`players`, `localPlayer`, `identities`, `onPlayerJoined/Left`, `onSpawned/Despawned`. Put a
`NetworkIdentity` on each prefab root; its `behaviours` list (auto-filled from children) fixes the
replication order; objects placed in the scene get a non-zero `sceneId` and are bound, not instantiated. Drop-in components: `NetworkTransform`, `NetworkRigidbody`, `NetworkAnimator`, `PlayerSpawner` (`Runtime/Framework/Components/`). `PlayerID` is the Aurix actor id. Modules and RPC methods are bound by reflection
in declaration order, so host and clients must run the same build. RPC methods are rewritten after
compilation by the weaver in `Editor/CodeGen` (an `ILPostProcessor`): calling
`CmdHit(10)` sends the RPC and the body runs where it should; without the weaver (or from code you do
not own) use `Rpc(nameof(CmdHit), 10)` / `RpcTo(player, …)`. Interest management: add `INetworkVisibilityRule`s to `NetworkManager.visibilityRules` (e.g. `new DistanceRule(manager, 50f)`) or set `NetworkIdentity.visibilityRule`; clients only hold objects they can see. Client-side prediction: derive from `PredictedBehaviour<INPUT, STATE>`, implement `GetInput`, a deterministic `Simulate(input, ref state, dt)` and `SetUnityState`; the owner predicts and reconciles, the server verifies, observers interpolate. Static RPCs work too: put `[ServerRpc]`/`[ObserversRpc]`/`[TargetRpc]` on a static method of any class (no NetworkIdentity needed); they route through `NetworkManager.main`. Lag compensation: add `ColliderRollback` to hittable objects and wrap server-side hit checks in `networkManager.Rollback(info.sender, () => Physics.Raycast(...))`. `StartAuto()` is quick match: join a random open room (`matchFilterJson`, `matchExpr`) or host a new one with `roomPropsJson`. Host migration is on by default: if the host leaves, the new room master becomes the server with the world it can see and the others re-attach.

One framework message (an RPC call, a state update of one object) holds up to 65535 bytes; a larger one
throws `NetException` where it is sent. `SyncBigData` streams larger payloads in chunks.

Check without Unity: `cd BuildCheck~/framework-check && NET_API_KEY=an_… dotnet run -c Release`;
with `NET_SLOW_HTTP` (a delaying proxy, see `sdk/cpp/README.md`) it also runs prediction over the slow link.
Like the transport check below, it runs on a single-threaded synchronization context, so the SDK's
`async` continuations come back to the thread that pumps `NetworkUpdate`, as they do in the engine.

More of the framework, all covered by `BuildCheck~/framework-check` against a live node: generic RPCs (`Show<T>(T v)`),
static RPCs on any class, `compression:`/`deltaPack:` RPC options, `StateMachine`/`StateNode<T>`, `NetworkLOD`
(distance send-rate tiers), `NetworkReflection` (sync members / call methods of any component by name), `NetworkBones`,
`NetworkAudioSource`, `NetworkTransform.adaptiveSync` + `syncParent`, `ValidatedSyncVar<T>` (server vetoes owner
writes), `SyncInput<T>`, `SyncBigData`/`SyncTexture`/`SyncFile`, `NetworkManager.statistics` (ping, jitter, throughput,
stall events; Window ▸ Aurix Net ▸ Network Statistics in the Editor) and `IAsyncNetworkObjectFactory` /
`AddressablesPrefabFactory` for prefabs that load on demand.

## PurrNet, Mirror and FishNet transports

Already on [PurrNet](https://github.com/PurrNet/PurrNet), [Mirror](https://github.com/MirrorNetworking/Mirror)
or [FishNet](https://github.com/FirstGearGames/FishNet)? Keep the framework (its `NetworkManager`,
SyncVars, RPCs, prediction) and move the wire to Aurix Net: `Runtime/Transports/RelayBridge.cs` runs
a host and its clients over one relay room, and a thin adapter per framework compiles when that
framework is in the project:

| Framework | Component | Compiles when |
|---|---|---|
| PurrNet | `AurixNetTransport` (`GenericTransport`/`ITransport`), `Runtime/PurrNet/` | package `dev.purrnet.purrnet` present → asmdef version define `AURIX_PURRNET` |
| Mirror | `AurixNetMirrorTransport` (`Transport`), `Runtime/Mirror/` | Mirror's own `MIRROR` define |
| FishNet | `AurixNetFishNetTransport` (`Transport`), `Runtime/FishNet/` | FishNet's own `FISHNET` define |

Add the component next to the framework's `NetworkManager`, point the manager's transport at it, and set:

| Field | Meaning |
|---|---|
| `httpUrl` | node control-plane URL (`https://…`, or `http://127.0.0.1:8090` locally) |
| `grant` / `GrantProvider` | grant from your backend (`POST /v1/grants`); `GrantProvider` is an async callback that wins over the field |
| `roomName`, `maxPlayers` | relay room shared by host and clients (Mirror `ClientConnect(address)` / FishNet `SetClientAddress` override the room name when non-empty) |
| `eventCode` | relay event code reserved for transport frames (default 200; keep other events off it) |

Topology: the framework's server joins or creates the relay room and must be its master; clients
join the same room and talk only to the master. The framework's connection id equals the Aurix
actor id. Reliable channels ride the reliable relay channel (fragmented above the datagram size,
MTU 8192), unreliable ones the unreliable channel (MTU = datagram payload); received frames are
handed on with the channel they were sent on (`AurixNetMirrorTransport`,
`AurixNetFishNetTransport`). Disconnecting a client from the server kicks the actor; the kicked
client sees the framework's server-side disconnect.
Host mode: with `AurixNetTransport` (`StartHost`) and `AurixNetFishNetTransport` (a client host)
the server and a client start on the same transport; that client is linked to the server
in-process (no second join, one grant), and the server sees it under its own actor id. With
`AurixNetMirrorTransport` the host uses the framework's own local connection; its `ServerUri()`
is `aurix://room/<room name>` and `ClientConnect(Uri)` accepts it.
Not yet: host migration (on `master_changed` the host stops and clients disconnect; reconnect from
the framework's side), lobby/matchmaking from inside the framework (use the Aurix REST/WS API or
`join_random` through a small wrapper).

Check without Unity: `cd BuildCheck~/transports-check && NET_API_KEY=an_… dotnet run -c Release`
runs host + two clients through each adapter against a local node, plus host mode for the two
adapters that have it (compile-only without the key). `NET_SLOW_HTTP` puts the delaying proxy
between the adapters and the node.

## Checks

`BuildCheck~/managed-tests` (`dotnet test`): offline NUnit tests of the managed core; the sim/nav tests load
`target/release/libnet_sim_wasm.so` (`cargo build --release -p net-sim-wasm`) and are skipped when it is absent.

`BuildCheck~/managed-live`: .NET console test of the managed core against a local node
(`NET_API_KEY=… dotnet run -c Release`): relay reliable/unreliable/fragmented events, sync deltas,
a custom-schema owner room (vec3/quat/string/u8 decode, acks with 0 gaps) and a lockstep room
(identical confirmed input streams on 3 clients, late join catch-up from the master's checkpoint). The Unity layer is
compiled there against `Stubs/UnityEngine.cs`; the real editor build is a separate step.
With `NET_SLOW_HTTP` set (a delaying proxy, see `sdk/cpp/README.md`) it also leaves a room and joins
another one over the slow link, sends two room lookups at once, and drives a `NetRunner` whose owner
moves every tick. With `NET_LOSSY_HTTP` set (the same proxy losing data frames, see
`sdk/cpp/README.md`) it also sends single writes over the lossy link. With `NET_PLAIN_HTTP` set
(the same proxy hiding the node's `supports`, see `sdk/cpp/README.md`) it also sends events for the
unsequenced channel that have to arrive on channel 0, from the managed client and, when
`libaurix_net` is on the library path (`LD_LIBRARY_PATH=target/release`), from the native one.
With `NET_CUT_HTTP` set (the same proxy closing the connection on `auth`, see `sdk/cpp/README.md`)
it also checks that `ConnectAsync` fails as the control channel closes.

Lockstep tuning: `Runner.Lockstep.InputDelay` adapts between `InputDelayMin`/`InputDelayMax` from the node's
input-margin reports (late inputs raise it at once to cover the round trip, steady slack lowers it again;
`AdaptiveDelay = false` to pin it), `InputRedundancy` (1–8) sets how many past inputs ride in each datagram. After a desync the
node asks for your state at the flagged tick: handle `OnDumpRequested(tick)` with `SendDump(tick, hash, state)`
from your rollback history; the dumps are downloadable via the REST API next to the master's.

## Dedicated server with a public IP

```csharp
// headless build on the machine that also runs an Aurix Net node
GetComponent<NetworkManager>().StartDedicated("http://127.0.0.1:8090");
```

The room lives on that node; clients keep the project URL and are redirected to your machine (`room_elsewhere`, followed automatically). See `docs/modes.md` → Topologies.

## Checking the package in an editor

`BuildCheck~/unity-project` is a Unity project that takes this package from its folder. Its
checks run with no manual steps:

```sh
BuildCheck~/unity-project/check.sh /path/to/Unity          # Linux, macOS
BuildCheck~/unity-project/check.ps1 C:\path\to\Unity.exe    # Windows
```

`Compile` passes when the package compiled and its post-processor rewrote the RPCs of a probe
behaviour; `BuildPlayer` builds a player of the editor's platform with the probe in its scene. A
second argument `web` adds a Web build. Each check prints one `AURIX <check> PASS|FAIL` line
and a `WARN` line for every warning of a build; the editor logs are in `Logs/`.

Unity 2022.3 gives every such build one warning, `GfxDevice renderer is null. Unity cannot
update the Ambient Probe and Reflection Probes that the SkyManager generates.` It comes from the
run without graphics (`-nographics`) and not from the package; Unity 6000.3 gives none.

The project holds only the packages its manifest names, so a module the package uses and does
not name fails the build. `Manifest` fails, and the check that ran is not judged, when the editor
has added anything to `Packages/manifest.json`. Every run resolves the packages
anew, without the lock file of the run before. Before a run with another version of the editor
delete the project's `Library`.