extends Node

var got := false
var t0 := 0

func fail(msg: String) -> void:
	printerr("GODOT FAIL: ", msg)
	get_tree().quit(1)

func _ready() -> void:
	for c in ["AurixNetClient", "AurixNetSync"]:
		if not ClassDB.class_exists(c):
			fail("class missing: " + c); return
	var http := OS.get_environment("NET_HTTP")
	if http == "": http = "http://127.0.0.1:8090"
	var ga := OS.get_environment("GRANT_A"); var gb := OS.get_environment("GRANT_B")
	var sync := AurixNetSync.new()
	if not sync.setup("component Transform {\n pos: vec3 @range(-4096, 4096, 0.01)\n}", 20, 2):
		fail("sync setup: " + sync.get_last_error()); return
	if ga == "" or gb == "":
		print("GODOT OK (classes load, sync schema parsed; no grants -> no live check)"); get_tree().quit(0); return
	var a := AurixNetClient.new(); var b := AurixNetClient.new(); add_child(a); add_child(b)
	if not a.connect_to(http, ga, true): fail("a connect: " + a.get_last_error()); return
	if not b.connect_to(http, gb, true): fail("b connect: " + b.get_last_error()); return
	var room := "godot-%d" % (Time.get_ticks_msec() % 100000)
	if not a.join_or_create(room, AurixNetClient.MODE_RELAY, 8): fail("a join: " + a.get_last_error()); return
	if not b.join_or_create(room, AurixNetClient.MODE_RELAY, 8): fail("b join: " + b.get_last_error()); return
	b.event.connect(func(code, from, payload): 
		print("b got event code=%d from=%d payload=%s" % [code, from, payload.get_string_from_utf8()])
		got = true)
	t0 = Time.get_ticks_msec()

func _process(_dt: float) -> void:
	if t0 == 0: return
	var a := get_child(0) as AurixNetClient
	if (Time.get_ticks_msec() - t0) % 200 < 20:
		a.send_event(1, "hi".to_utf8_buffer()); a.flush()
	if got:
		print("GODOT OK: joined=", a.get_joined_json().substr(0, 60)); get_tree().quit(0); t0 = 0
	elif Time.get_ticks_msec() - t0 > 10000:
		fail("no event within 10 s; a.err=" + a.get_last_error()); t0 = 0
