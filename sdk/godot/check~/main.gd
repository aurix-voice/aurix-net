extends Node

var got := false
var t0 := 0
const SCHEMA := "component Transform {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n}\n"
var grant_c := ""; var grant_d := ""; var net_http := ""
var c: AurixNetClient; var d: AurixNetClient
var wc: AurixNetSync; var wd: AurixNetSync
var sync_on := false; var acc := 0.0; var writes := 0; var updates := 0; var x := 0.0; var last := Vector3.ZERO; var drain := 0

func fail(msg: String) -> void:
	printerr("GODOT FAIL: ", msg)
	get_tree().quit(1)

func _ready() -> void:
	for c in ["AurixNetClient", "AurixNetSync"]:
		if not ClassDB.class_exists(c):
			fail("class missing: " + c); return
	var http := OS.get_environment("NET_HTTP")
	if http == "": http = "http://127.0.0.1:8090"
	net_http = http
	var ga := OS.get_environment("GRANT_A"); var gb := OS.get_environment("GRANT_B")
	grant_c = OS.get_environment("GRANT_C"); grant_d = OS.get_environment("GRANT_D")
	var sync := AurixNetSync.new()
	if not sync.setup("component Transform {\n pos: vec3 @range(-4096, 4096, 0.01)\n}", 20, 2):
		fail("sync setup: " + sync.get_last_error()); return
	if ga == "" or gb == "":
		print("GODOT OK (classes load, sync schema parsed; no grants -> no live check)"); get_tree().quit(0); return
	var a := AurixNetClient.new(); var b := AurixNetClient.new(); add_child(a); add_child(b)
	if not a.connect_to(http, ga, true): fail("a connect: " + a.get_last_error()); return
	if not b.connect_to(http, gb, true): fail("b connect: " + b.get_last_error()); return
	var room := "godot-%d" % (Time.get_ticks_msec() % 100000)
	if a.join_or_create(room, AurixNetClient.MODE_RELAY, 8) < 0: fail("a join: " + a.get_last_error()); return
	if b.join_or_create(room, AurixNetClient.MODE_RELAY, 8) < 0: fail("b join: " + b.get_last_error()); return
	b.event.connect(func(code, from, payload): 
		print("b got event code=%d from=%d payload=%s" % [code, from, payload.get_string_from_utf8()])
		got = true)
	t0 = Time.get_ticks_msec()

func _process(dt: float) -> void:
	if sync_on: _sync_step(dt); return
	if t0 == 0: return
	var a := get_child(0) as AurixNetClient
	if (Time.get_ticks_msec() - t0) % 200 < 20:
		a.send_event(1, "hi".to_utf8_buffer()); a.flush()
	if got:
		print("relay ok: joined=", a.get_joined_json().substr(0, 60)); t0 = 0; _start_sync()
	elif Time.get_ticks_msec() - t0 > 10000:
		fail("no event within 10 s; a.err=" + a.get_last_error()); t0 = 0

func _start_sync() -> void:
	if grant_c == "" or grant_d == "":
		print("GODOT OK (relay only; set GRANT_C/GRANT_D for the sync check)"); get_tree().quit(0); return
	c = AurixNetClient.new(); d = AurixNetClient.new(); add_child(c); add_child(d)
	if not c.connect_to(net_http, grant_c, true): fail("c connect: " + c.get_last_error()); return
	if not d.connect_to(net_http, grant_d, true): fail("d connect: " + d.get_last_error()); return
	var props := JSON.stringify({"schema": SCHEMA})
	var room := "godot-sync-%d" % (Time.get_ticks_msec() % 100000)
	if c.join_or_create(room, AurixNetClient.MODE_SYNC, 4, props) < 0: fail("c join: " + c.get_last_error()); return
	if d.join_or_create(room, AurixNetClient.MODE_SYNC, 4, props) < 0: fail("d join: " + d.get_last_error()); return
	wc = AurixNetSync.new(); wd = AurixNetSync.new()
	if not wc.setup(SCHEMA, 20, 2) or not wd.setup(SCHEMA, 20, 2): fail("sync setup: " + wd.get_last_error()); return
	c.data.connect(func(_ch, _tick, payload): wc.receive(c, payload))
	d.data.connect(func(_ch, _tick, payload): wd.receive(d, payload))
	wd.change.connect(func(entity, owner):
		if owner == c.get_actor():
			updates += 1; last = wd.get_field(entity, "Transform", "pos"))
	sync_on = true

func _sync_step(dt: float) -> void:
	acc += dt
	if acc < 0.016: return
	acc = 0.0
	wd.advance(0.016)
	if writes < 90:
		writes += 1; x += 0.5
		if not wc.begin_write("Transform"): fail("begin_write: " + wc.get_last_error()); sync_on = false; return
		wc.write_field("pos", Vector3(x, 1, -2)); wc.write_field("rot", Quaternion.IDENTITY); wc.end_write()
		wc.send_writes(c); c.flush()
		return
	drain += 1
	if drain < 20: return
	sync_on = false
	var ok: bool = wd.get_server_tick() > 30 and wd.get_gaps() == 0 and updates >= 20 and abs(last.x - x) < 3 and abs(last.y - 1) < 0.02 and abs(last.z + 2) < 0.02 and wd.get_entities().size() >= 1
	print("godot sync: serverTick=%d gaps=%d updates=%d last=%s x=%.1f entities=%d renderTick=%.1f -> %s" % [wd.get_server_tick(), wd.get_gaps(), updates, last, x, wd.get_entities().size(), wd.get_render_tick(), "ok" if ok else "FAIL"])
	if ok: print("GODOT OK"); get_tree().quit(0)
	else: fail("sync replication")
