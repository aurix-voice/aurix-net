# Aurix Net for Godot 4 (GDExtension)

Build: `git clone -b 4.2 https://github.com/godotengine/godot-cpp` here, `cargo build --release -p net-ffi`
in the repo root, then `scons target=template_debug`. Copy `bin/` + `aurix_net.gdextension` to
`addons/aurix_net/` together with `libaurix_net.so` / `aurix_net.dll`.

```gdscript
var net := AurixNetClient.new(); add_child(net)   # polls in _process
net.event.connect(func(code, from, payload): print(code, from, payload))
net.connect_to("https://<node>", grant_from_backend)
net.join_or_create("lobby-1", AurixNetClient.MODE_RELAY, 8)
net.send_event(1, "hi".to_utf8_buffer()); net.flush()
```

## Signals added for operator messages

- `notice(text: String, level: String)` — system message pushed by the operator console or the game backend (`level` is `info` or `warn`). Show it in the HUD; it is not a chat message.
- `rehost()` — the room moved to another node during a rolling deploy. Call `rejoin()` with the same grant (the node list from `/v1/regions` works as fallback).

The C++ header exposes the same as `onNotice` / `onRehost` callbacks.

## aurix-sim

`libaurix_net` also carries the deterministic simulation (`src/aurix_sim.h`: 2D/3D physics, navmesh,
crowd, `sim3_*` convex hulls and casts). Call it from your GDExtension code the same way the client
header is used; values are raw Q32.32 (`aurix_fx_from_double`), handles/ids match the Web and Unity
wrappers, and `sim_golden_hash()` etc. let you assert the build is bit-identical to the other SDKs.
