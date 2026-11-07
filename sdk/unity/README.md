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
