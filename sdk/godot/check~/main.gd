extends Node

var got := false
var unsequenced := false
var t0 := 0
# NET_LIVE_REQUIRED: a part that cannot run fails the check instead of being skipped
var required := OS.get_environment("NET_LIVE_REQUIRED") != ""
const FRAME := 1.0 / 60
const SCHEMA := "component Transform {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n}\n"
var grant_c := ""; var grant_d := ""; var net_http := ""; var ls_room := ""
var c: AurixNetClient; var d: AurixNetClient
var wc: AurixNetSync; var wd: AurixNetSync
var sync_on := false; var acc := 0.0; var writes := 0; var updates := 0; var x := 0.0; var last := Vector3.ZERO; var drain := 0; var spawn_tag := -1; var spawned := -1; var first := PackedByteArray()
var lc: AurixNetLockstep; var ld: AurixNetLockstep
var ls_on := false; var ls_frames := 0; var ticks_c := 0; var ticks_d := 0; var actors := {}; var stray := 0; var command := 0
var carried := 0; var refusals: Array[String] = []

func fail(msg: String) -> void:
	printerr("GODOT FAIL: ", msg)
	get_tree().quit(1)

func _delta(tick: int) -> PackedByteArray:
	return PackedByteArray([0x11, 0, 0, tick >> 8, tick & 255, 0, 0, 0, 0, 0, 0, 0, 0])

# no network: deltas with no entities to a world with no client behind it, late in one run and from
# a room that stood still in the other
func _late_deltas() -> String:
	var world := AurixNetSync.new(); world.setup("component T {\n x: f32\n}", 60, 2)
	world.receive(null, _delta(10)); world.advance(FRAME)
	var ahead := 0.0
	for i in 3: ahead = max(ahead, world.advance(FRAME) - world.get_server_tick())
	for tick in range(11, 15): world.receive(null, _delta(tick))
	if ahead > 0 or not is_equal_approx(world.get_render_tick(), 12): return "late deltas: %.1f ahead of serverTick=10, then %.1f at serverTick=14" % [ahead, world.get_render_tick()]
	return ""

func _stood_still() -> String:
	var world := AurixNetSync.new(); world.setup("component T {\n x: f32\n}", 60, 2)
	world.receive(null, _delta(10))
	for i in 7: world.advance(FRAME)
	if world.get_render_tick() != 10: return "room stood still: %.1f at serverTick=10" % world.get_render_tick()
	for tick in range(11, 21):
		world.receive(null, _delta(tick))
		if not is_equal_approx(world.get_render_tick(), tick - 2) or world.advance(FRAME) > tick: return "room stood still: %.1f at serverTick=%d" % [world.get_render_tick(), tick]
	return ""

func _ready() -> void:
	for c in ["AurixNetClient", "AurixNetSync", "AurixNetLockstep"]:
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
	var ahead := [_late_deltas(), _stood_still()].filter(func(found): return found != "")
	if not ahead.is_empty(): fail("render tick, " + ", ".join(ahead)); return
	if ga == "" or gb == "":
		if required: fail("NET_LIVE_REQUIRED is set and GRANT_A, GRANT_B are not"); return
		print("GODOT OK (classes load, sync schema parsed, render tick behind the room; no grants -> no live check)"); get_tree().quit(0); return
	var a := AurixNetClient.new(); var b := AurixNetClient.new(); add_child(a); add_child(b)
	if not a.connect_to(http, ga, true): fail("a connect: " + a.get_last_error()); return
	if not b.connect_to(http, gb, true): fail("b connect: " + b.get_last_error()); return
	var room := "godot-%d" % OS.get_process_id()
	# a seat held for a player on its way is not given to another
	if a.join_or_create(room + "-held", AurixNetClient.MODE_RELAY, 2, "", PackedStringArray(), PackedStringArray(["godot-awaited"])) < 0: fail("a join: " + a.get_last_error()); return
	if b.join(room + "-held") >= 0 or b.get_last_error_code() != AurixNetClient.ERROR_ROOM_FULL: fail("b took a held seat: " + b.get_last_error()); return
	# a message with a field the node does not know is refused before it is sent, and the field is named
	if b.send_control(JSON.stringify({"t": "set_groups", "join": [1], "colour": "red"})) or b.get_last_error_code() != AurixNetClient.ERROR_INVALID_REQUEST or not b.get_last_error().contains("unknown field `colour`"): fail("b sent a field the node does not know: " + b.get_last_error()); return
	a.leave()
	# a hidden room is not offered to a random join and is entered by its name
	if a.join_or_create(room + "-hidden", AurixNetClient.MODE_RELAY, 4, JSON.stringify({"map": room + "-hidden"}), PackedStringArray(["map"]), PackedStringArray(), false) < 0: fail("a hidden room: " + a.get_last_error()); return
	if b.join_random("", "map = '%s-hidden'" % room) >= 0 or b.get_last_error_code() != AurixNetClient.ERROR_NOT_FOUND: fail("b was offered a hidden room: " + b.get_last_error()); return
	if b.join(room + "-hidden") < 0: fail("b hidden room by name: " + b.get_last_error()); return
	a.leave(); b.leave()
	# a closed room lets its creator in and nobody else
	if a.join_or_create(room + "-closed", AurixNetClient.MODE_RELAY, 4, "", PackedStringArray(), PackedStringArray(), true, false) < 0: fail("a closed room: " + a.get_last_error()); return
	if b.join(room + "-closed") >= 0 or b.get_last_error_code() != AurixNetClient.ERROR_ROOM_CLOSED: fail("b joined a closed room: " + b.get_last_error()); return
	if b.join_or_create(room + "-closed") >= 0 or b.get_last_error_code() != AurixNetClient.ERROR_ROOM_CLOSED: fail("b joined a closed room by join_or_create: " + b.get_last_error()); return
	a.leave()
	# a random join finds a room by the props it publishes
	if a.join_or_create(room, AurixNetClient.MODE_RELAY, 8, JSON.stringify({"map": room}), PackedStringArray(["map"])) < 0: fail("a join: " + a.get_last_error()); return
	if b.join_random("", "map = '%s'" % room) < 0: fail("b random join: " + b.get_last_error()); return
	# the room as the node described it at the join: its size, props and who was in it, the joiner too
	var facts: Dictionary = JSON.parse_string(b.get_joined_json())
	if int(facts.get("max_players", 0)) != 8 or facts.get("props", {}).get("map") != room or facts.get("players", []).size() != 2: fail("joined: " + b.get_joined_json()); return
	# each client knows the player its grant names, the one the room lists it as
	var ids: Array = facts.get("players", []).map(func(player): return player.get("player_id"))
	if a.get_player_id() == "" or a.get_player_name() == "" or a.get_player_id() == b.get_player_id() or not ids.has(a.get_player_id()) or not ids.has(b.get_player_id()): fail("player: a %s, b %s, room %s" % [a.get_player_id(), b.get_player_id(), ids]); return
	b.event.connect(func(code, from, payload): 
		print("b got event code=%d from=%d payload=%s" % [code, from, payload.get_string_from_utf8()])
		got = true)
	b.data.connect(func(channel, _tick, _payload):
		if channel == AurixNetClient.CHANNEL_UNRELIABLE_UNSEQUENCED: unsequenced = true)
	t0 = Time.get_ticks_msec()

func _process(dt: float) -> void:
	if sync_on: _sync_step(dt); return
	if ls_on: _lockstep_step(); return
	if t0 == 0: return
	var a := get_child(0) as AurixNetClient
	if (Time.get_ticks_msec() - t0) % 200 < 20:
		a.send_event(1, "hi".to_utf8_buffer())
		a.send_event_on(AurixNetClient.CHANNEL_UNRELIABLE_UNSEQUENCED, 6, "hi".to_utf8_buffer()); a.flush()
	if got and unsequenced:
		print("relay ok: joined=", a.get_joined_json().substr(0, 60)); t0 = 0; _start_sync()
	elif Time.get_ticks_msec() - t0 > 10000:
		fail("event=%s, event on the unsequenced channel=%s within 10 s; a.err=%s" % [got, unsequenced, a.get_last_error()]); t0 = 0

func _start_sync() -> void:
	if grant_c == "" or grant_d == "":
		if required: fail("NET_LIVE_REQUIRED is set and GRANT_C, GRANT_D are not"); return
		print("GODOT OK (relay only; set GRANT_C/GRANT_D for the sync check)"); get_tree().quit(0); return
	c = AurixNetClient.new(); d = AurixNetClient.new(); add_child(c); add_child(d)
	if not c.connect_to(net_http, grant_c, true): fail("c connect: " + c.get_last_error()); return
	if not d.connect_to(net_http, grant_d, true): fail("d connect: " + d.get_last_error()); return
	var props := JSON.stringify({"schema": SCHEMA})
	var room := "godot-sync-%d" % OS.get_process_id()
	if c.join_or_create(room, AurixNetClient.MODE_SYNC, 4, props) < 0: fail("c join: " + c.get_last_error()); return
	if d.join_or_create(room, AurixNetClient.MODE_SYNC, 4, props) < 0: fail("d join: " + d.get_last_error()); return
	wc = AurixNetSync.new(); wd = AurixNetSync.new()
	if not wc.setup(SCHEMA, 20, 2) or not wd.setup(SCHEMA, 20, 2): fail("sync setup: " + wd.get_last_error()); return
	c.data.connect(func(_ch, _tick, payload):
		if ls_on: lc.receive(c, payload)
		else: wc.receive(c, payload))
	d.data.connect(func(_ch, _tick, payload):
		if ls_on: ld.receive(d, payload); return
		var empty := wd.get_server_tick() == 0
		wd.receive(d, payload)
		if empty and wd.get_server_tick() != 0: first = payload)
	# sync and lockstep frames are not relay events: the event signal must stay quiet in both rooms
	c.event.connect(func(_code, _from, _payload): stray += 1)
	d.event.connect(func(_code, _from, _payload): stray += 1)
	wd.change.connect(func(entity, owner):
		if owner == c.get_actor():
			updates += 1; last = wd.get_field(entity, "Transform", "pos"))
	wc.spawn_answer.connect(func(tag, status, entity, _tick):
		if tag == spawn_tag and status == 0: spawned = entity)
	spawn_tag = wc.spawn_entity(c)
	if spawn_tag < 1: fail("spawn_entity: the room does not answer tagged spawns"); return
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
	var ok: bool = wd.get_server_tick() > 30 and wd.get_gaps() == 0 and updates >= 20 and abs(last.x - x) < 0.02 and abs(last.y - 1) < 0.02 and abs(last.z + 2) < 0.02 and wd.get_entities().size() >= 1
	print("godot sync: serverTick=%d gaps=%d updates=%d last=%s x=%.1f entities=%d renderTick=%.1f -> %s" % [wd.get_server_tick(), wd.get_gaps(), updates, last, x, wd.get_entities().size(), wd.get_render_tick(), "ok" if ok else "FAIL"])
	if not ok: fail("sync replication"); return
	# the room told which entity the spawn made and which one is the avatar
	if spawned < 0 or c.get_avatar() < 0 or c.get_avatar() == spawned or not wc.get_entities().has(spawned):
		fail("spawn answer: tag=%d entity=%d avatar=%d" % [spawn_tag, spawned, c.get_avatar()]); return
	if stray > 0: fail("%d event signals from sync frames" % stray); return
	# a replay: the room's first delta again, to a world with no client behind it
	var cam := AurixNetSync.new(); var shown := []
	cam.spawn.connect(func(entity, _owner): shown.append(entity))
	if not cam.setup(SCHEMA, 20, 2) or not cam.receive(null, first) or shown.is_empty():
		fail("replay: %d entities from a frame with no client" % shown.size()); return
	_start_lockstep()

func _start_lockstep() -> void:
	c.leave(); d.leave()
	if c.get_actor() != -1 or c.get_joined_json() != "": fail("leave: the client still describes the room it left"); return
	ls_room = "godot-ls-%d" % OS.get_process_id()
	var room := ls_room
	var props := JSON.stringify({"tick_hz": 30})
	if c.join_or_create(room, AurixNetClient.MODE_LOCKSTEP, 4, props) < 0: fail("c lockstep join: " + c.get_last_error()); return
	if d.join_or_create(room, AurixNetClient.MODE_LOCKSTEP, 4, props) < 0: fail("d lockstep join: " + d.get_last_error()); return
	if c.get_seed() == 0 or c.get_seed() != d.get_seed(): fail("lockstep seed: %d and %d" % [c.get_seed(), d.get_seed()]); return
	lc = AurixNetLockstep.new(); ld = AurixNetLockstep.new()
	lc.tick.connect(func(_tick, inputs):
		ticks_c += 1
		for i in inputs: actors[i["actor"]] = true)
	ld.tick.connect(func(_tick, _inputs): ticks_d += 1)
	ld.command.connect(func(_tick, _actor, data):
		if data.size() == 251: command = 251
		else: carried += 1)
	c.control.connect(func(json):
		var message: Dictionary = JSON.parse_string(json)
		if message.get("t") == "error": refusals.append("%s: %s" % [message.get("code"), message.get("message")]))
	# the longest command reaches the other player whole; a longer one and an empty one are refused
	var longest := PackedByteArray(); longest.resize(251)
	if not lc.send_command(c, longest) or lc.send_command(c, longest + PackedByteArray([0])) or lc.send_command(c, PackedByteArray()): fail("send_command: the limit"); return
	ls_on = true

func _lockstep_step() -> void:
	ls_frames += 1
	if ls_frames <= 240:
		# every frame, more often than the 30 Hz room ticks: extra calls return -1 and send nothing
		lc.send_input(c, PackedByteArray([ls_frames % 200])); ld.send_input(d, PackedByteArray([ls_frames % 200]))
		if ls_frames == 60:
			# a command the node refuses comes back as a control error: one sent past the sender's
			# check, then 40 of 200 bytes in one go, of which a tick carries one and 4096 bytes may wait
			var too_long := PackedByteArray([0x28]); too_long.resize(253)
			c.send_data(AurixNetClient.CHANNEL_RELIABLE, too_long)
			for i in 40:
				var flood := PackedByteArray(); flood.resize(200); flood.fill(i)
				lc.send_command(c, flood)
		c.flush(); d.flush()
		return
	if ls_frames < 270: return
	ls_on = false
	var ahead: int = lc.get_server_tick() - lc.get_confirmed_tick()
	var limited := refusals.count("rate_limited: rate limited")
	var refused: bool = refusals.size() > 1 and refusals[0] == "invalid_request: invalid request: a lockstep command takes 1 to 251 bytes, got 252" \
		and limited == refusals.size() - 1 and carried + limited == 40
	var ok: bool = ticks_c > 20 and ticks_d > 20 and actors.size() == 2 and stray == 0 and ahead < 10 and command == 251 and refused
	print("godot lockstep: ticks=%d/%d actors=%d event signals=%d confirmed=%d server=%d commands carried=%d refused=%d -> %s" % [ticks_c, ticks_d, actors.size(), stray, lc.get_confirmed_tick(), lc.get_server_tick(), carried, refusals.size(), "ok" if ok else "FAIL"])
	if not ok: fail("lockstep"); return
	if not await _replaced(): fail("replaced: the client still describes the room"); return
	if not await _slow_link(): fail("slow link"); return
	if await _rehost(): print("GODOT OK"); get_tree().quit(0)
	else: fail("rehost")

# The same player joins from a second connection: the node takes the first one out of the room.
func _replaced() -> bool:
	var second := AurixNetClient.new(); add_child(second)
	if not second.connect_to(net_http, grant_d, true) or second.join(ls_room) < 0: return false
	for i in 100:
		if d.get_actor() == -1: break
		await get_tree().create_timer(0.01).timeout
	return d.get_actor() == -1 and d.get_joined_json() == "" and d.get_seed() == 0

# Over a slow link (NET_SLOW_HTTP with GRANT_E and GRANT_F, see README): a client that left a room joins another one.
func _slow_link() -> bool:
	var slow := OS.get_environment("NET_SLOW_HTTP")
	if slow == "":
		print("godot slow link: skipped (NET_SLOW_HTTP not set)"); return not required
	var e := AurixNetClient.new(); var f := AurixNetClient.new(); add_child(e); add_child(f)
	if not e.connect_to(slow, OS.get_environment("GRANT_E"), true) or not f.connect_to(slow, OS.get_environment("GRANT_F"), true): return false
	var errors := [0]; var next := [0]
	e.control.connect(func(json): if json.contains('"t":"error"'): errors[0] += 1)
	f.event.connect(func(code, _from, _payload): if code == 5: next[0] += 1)
	var room := "godot-slow-%d" % OS.get_process_id()
	if e.join_or_create(room, AurixNetClient.MODE_RELAY, 4) < 0 or f.join_or_create(room, AurixNetClient.MODE_RELAY, 4) < 0: return false
	# events on their way to `e` when it leaves must not make it answer the room it has left
	for i in 60:
		if i == 30: e.leave()
		f.send_event(4, PackedByteArray([9]), AurixNetClient.TARGET_OTHERS, true); f.flush()
		await get_tree().create_timer(0.01).timeout
	var joined := e.join_or_create(room + "-2", AurixNetClient.MODE_RELAY, 4) > 0
	f.leave()
	joined = joined and f.join(room + "-2") > 0
	if joined:
		for i in 5: e.send_event(5, PackedByteArray([i]), AurixNetClient.TARGET_OTHERS, true)
		e.flush()
		var until := Time.get_ticks_msec() + 2000
		while Time.get_ticks_msec() < until and next[0] < 5: await get_tree().process_frame
	print("godot slow link: joined=%s errors after leave=%d reliable events in the next room=%d/5" % [joined, errors[0], next[0]])
	return joined and errors[0] == 0 and next[0] == 5

# Two nodes (NET_REHOST_HTTP is the second one, GRANT_G and GRANT_H, see README): the first node shuts down and hands the room over.
func _rehost() -> bool:
	var other := OS.get_environment("NET_REHOST_HTTP")
	if other == "":
		print("godot rehost: skipped (NET_REHOST_HTTP not set)"); return not required
	var grant := OS.get_environment("GRANT_G")
	var g := AurixNetClient.new(); add_child(g)
	if not g.connect_to(OS.get_environment("NET_HTTP"), grant): return false
	var room := OS.get_environment("NET_REHOST_ROOM")
	var actor := g.join_or_create(room, AurixNetClient.MODE_RELAY, 8)
	# a client that asks the second node for the room is sent to the node that hosts it
	var o := AurixNetClient.new(); add_child(o)
	var there := o.connect_and_join(other, OS.get_environment("GRANT_H"), JSON.stringify({"t": "join", "name": room}))
	o.disconnect_from()
	var moved := [false]
	g.rehost.connect(func(): moved[0] = true)
	var until := Time.get_ticks_msec() + 60000
	while Time.get_ticks_msec() < until and not moved[0]: await get_tree().process_frame
	var forgot := g.get_actor() == -1 and g.get_joined_json() == ""
	g.disconnect_from()
	var again := -1
	if g.connect_to(other, grant): again = g.join(room)
	print("godot rehost: asked the other node=%d moved=%s forgot the room=%s actor=%d rejoined as %d" % [there, moved[0], forgot, actor, again])
	return there > 0 and there != actor and moved[0] and forgot and actor > 0 and again == actor
