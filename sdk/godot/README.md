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
