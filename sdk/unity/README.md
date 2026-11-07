# Aurix Net for Unity

UPM package over `libaurix_net` (C ABI). Put the native library for each platform under
`Plugins/` (`aurix_net.dll`, `libaurix_net.so`, `libaurix_net.dylib`, iOS static lib).

```csharp
var client = Aurix.Net.Client.Connect("https://<node>", grantFromYourBackend);
client.OnData += (ch, tick, bytes) => { if (Aurix.Net.EventFrame.TryDecode(bytes, out var f)) Handle(f); };
client.OnControl += json => Debug.Log(json);
client.JoinOrCreate("lobby-1", Aurix.Net.RoomMode.Relay, 8);
// per frame:
client.Poll();
client.SendEvent(1, payload);
client.Flush();
```

Grants come from your backend (`POST /v1/grants` with an API key); the key never ships in
the build. Sync mode: send inputs with `SendData(Channel.Unreliable, inputFrame)` and apply
`0x11` deltas; lockstep: `0x20` bundles / `0x21` hash reports (see `docs/modes.md`).

## Two transports

- **Managed (default, `Aurix.Net.Managed.ManagedClient`)**: pure C#, no native library. Control
  over WebSocket, data over the WebSocket tunnel with the same packet format, channels and
  fragmentation as QUIC. Works on every platform Unity builds for, including WebGL.
- **Native (`Aurix.Net.Client`)**: P/Invoke into `libaurix_net` for QUIC datagrams (lower
  latency on lossy links). Needs the native binary per platform; not for WebGL.

```csharp
var c = await ManagedClient.ConnectAsync("https://net.example.com", grant);
c.OnEvent += f => Debug.Log($"event {f.Code} from {f.From}");
await c.JoinOrCreateAsync("lobby-1", "relay", 8);
c.SendEvent(1, Encoding.UTF8.GetBytes("hi"), Target.Others, reliable: true);
void Update() { c.Poll(); }           // dispatch on the main thread
void LateUpdate() { c.Flush(); }      // push the unreliable batch
```

`BuildCheck~/managed-live` is a .NET console test of the managed client against a local node
(`NET_API_KEY=… dotnet run`): relay reliable/unreliable/fragmented events and sync deltas.
