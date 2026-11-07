# Aurix Net for Unity

Pure C#, no native library, every Unity platform including WebGL. Install as a UPM package
(`Packages/manifest.json` → `"com.aurix.net": "file:../../sdk/unity"` or a git URL).

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
Runner.OnOwnEntityUpdated += (view, entity, tick) => { if (view == View) pred.Reconcile(tick, ReadPos(entity)); };
void Tick(byte[] input) { uint t = Runner.SendInput(input); transform.position = pred.Record(t, input) + _visualError; }
```

`Move` must be the rule your WASM logic applies for one tick of that input (same fixed-point/float math); a
mismatch is corrected by replaying the unconfirmed inputs on the server state (`Mispredictions` counts them).
Owner-authoritative rooms (no `logic`) do not need prediction: the owner's `[Sync]` values are the truth.

Relay rooms (no simulation): `Runner.SendEvent(code, bytes, Target.All, reliable: true)` and
`Runner.OnEvent`. Event codes `0x10–0x13` and `0x20–0x24` are reserved for sync/lockstep frames.

Lockstep rooms (deterministic, every client simulates): `Runner.Lockstep` delivers confirmed inputs
per tick in order (`OnTick`), restores state on late join / rehost (`OnRestore`), uploads the master's
checkpoint when the node asks (`OnCheckpointRequested` → `SendCheckpoint`); you send `SendInput(bytes)`
every tick and `ReportHash(tick, hash)` so the node can flag desyncs. Keep the simulation integer or
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

## Checks

`BuildCheck~/managed-live`: .NET console test of the managed core against a local node
(`NET_API_KEY=… dotnet run -c Release`): relay reliable/unreliable/fragmented events, sync deltas,
a custom-schema owner room (vec3/quat/string/u8 decode, acks with 0 gaps) and a lockstep room
(identical confirmed input streams on 3 clients, late join catch-up from the master's checkpoint). The Unity layer is
compiled there against `Stubs/UnityEngine.cs`; the real editor build is a separate step.
