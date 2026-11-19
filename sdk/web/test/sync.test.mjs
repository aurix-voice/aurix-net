// Offline checks of the sync codec against golden vectors produced by the managed C# core
// (sdk/unity/BuildCheck~/managed-tests asserts the same bytes): node --test test/
import { test } from "node:test";
import assert from "node:assert/strict";
import { BitWriter, BitReader, Schema, ComponentValue, SyncWorld, SyncSession, InterpolationBuffer, lerpVec, distVec } from "../dist/index.js";

const hex = (b) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
const SCHEMA = "component Transform {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n}\ncomponent Stats {\n  hp: u16\n  name: string(16)\n  speed: f32\n  team: i8\n  flags: bits(5)\n  seed: u64\n}\n";

test("schema parses like the managed core", () => {
  const s = Schema.parse(SCHEMA);
  assert.equal(s.components.length, 2);
  assert.equal(s.componentIdBits, 1);
  assert.deepEqual(s.byName("Transform").fields.map((f) => f.type.kind), ["vec3", "quat"]);
  assert.deepEqual(s.byName("Stats").fields.map((f) => f.type.kind), ["uint", "string", "f32", "int", "uint", "uint"]);
  assert.equal(s.byName("Stats").fields[4].type.bits, 5);
  assert.throws(() => Schema.parse("component A {\n x: vec3\n}"), /needs @range/);
  assert.throws(() => Schema.parse("component A {\n x: u8\n"), /unterminated/);
});

test("bit writer matches the C# vector", () => {
  const w = new BitWriter(); w.writeBits(0x1ff, 9); w.writeInt(-77, 12); w.writeQuantized(12.345, -100, 100, 0.001); w.writeF32(1.5); w.writeBits((1n << 64n) - 1n, 64); w.writeBool(true);
  assert.equal(hex(w.finish()), "ff84cb6db27f800001ffffffffffffffff");
  const r = new BitReader(w.finish());
  assert.equal(r.readBits(9), 0x1ff); assert.equal(r.readInt(12), -77n); assert.ok(Math.abs(r.readQuantized(-100, 100, 0.001) - 12.345) < 0.001);
  assert.equal(r.readF32(), 1.5); assert.equal(r.readBig(64), (1n << 64n) - 1n); assert.equal(r.readBool(), true);
});

test("owner writes match the C# vector and round-trip", () => {
  const s = Schema.parse(SCHEMA); const w = new SyncWorld(s);
  const tr = s.byName("Transform"), st = s.byName("Stats");
  const pos = new ComponentValue([[1.25, -2.5, 300.75], [0.1, 0.2, 0.3, 0.9]]);
  const stats = new ComponentValue([4242n, "zé", 3.5, -3n, 21n, 0x0123456789abcdefn]);
  const body = w.encodeWrites([[tr, pos], [st, stats]]);
  assert.equal(hex(body), "023203eb1f8335abdf255296f442486f5875280c000000b50123456789abcdef");
  const r = new BitReader(body); assert.equal(r.readBits(8), 2); assert.equal(r.readBits(s.componentIdBits), tr.id);
  const p2 = ComponentValue.decodeFull(tr, r);
  assert.ok(p2.fields[0].every((x, i) => Math.abs(x - pos.fields[0][i]) < 0.011));
  const q = p2.fields[1]; const n = Math.hypot(0.1, 0.2, 0.3, 0.9); assert.ok(q.every((x, i) => Math.abs(x - pos.fields[1][i] / n) < 0.002));
  assert.equal(r.readBits(s.componentIdBits), st.id);
  const s2 = ComponentValue.decodeFull(st, r);
  assert.deepEqual(s2.fields, [4242n, "zé", 3.5, -3n, 21n, 0x0123456789abcdefn]);
});

test("input and ack framing match the C# vectors", () => {
  assert.equal(hex(SyncWorld.encodeInput(0x01020304, [Uint8Array.of(1, 2), new Uint8Array(0), Uint8Array.of(9)])), "100102030403020102000109");
  assert.equal(hex(SyncWorld.encodeAck(0xdeadbeef, 0x80000001)), "13deadbeef80000001");
});

test("delta decoding: spawn, change (partial), despawn, acks and gap detection", () => {
  const s = Schema.parse(SCHEMA); const w = new SyncWorld(s); const tr = s.byName("Transform");
  const events = []; w.onSpawn = (e) => events.push(["spawn", e.id]); w.onChange = (e) => events.push(["change", e.id]); w.onDespawn = (e) => events.push(["despawn", e.id]);
  const delta = (tick, baseline, despawned, ents) => {
    const b = new BitWriter(); b.writeBits(tick, 32); b.writeBits(baseline, 32); b.writeBits(despawned.length, 16); for (const d of despawned) b.writeBits(d, 48);
    b.writeBits(ents.length, 16);
    for (const e of ents) { b.writeBits(e.id, 48); b.writeBool(e.owner != null); if (e.owner != null) b.writeBits(e.owner, 16); b.writeBits(e.comps.length, 8); for (const c of e.comps) { b.writeBits(c.def.id, s.componentIdBits); b.writeBool(false); b.writeBool(c.full); if (c.full) c.value.encodeFull(c.def, b); else { c.changed.forEach((ch) => b.writeBool(ch)); c.changed.forEach((ch, i) => { if (ch) ComponentValueWrite(b, c.def.fields[i].type, c.value.fields[i]); }); } } }
    const body = b.finish(); const out = new Uint8Array(body.length + 1); out[0] = 0x11; out.set(body, 1); return out;
  };
  const ComponentValueWrite = (b, t, v) => new ComponentValue([v]).encodeFull({ fields: [{ name: "x", type: t }] }, b);
  const full = new ComponentValue([[1, 2, 3], [0, 0, 0, 1]]);
  let ack = w.receive(delta(10, 0, [], [{ id: 7, owner: 3, comps: [{ def: tr, full: true, value: full }] }]));
  assert.equal(hex(ack), hex(SyncWorld.encodeAck(10, 0)));
  assert.equal(w.serverTick, 10); assert.equal(w.entities.get(7).owner, 3);
  ack = w.receive(delta(11, 11, [], [{ id: 7, owner: 3, comps: [{ def: tr, full: false, changed: [true, false], value: new ComponentValue([[4, 5, 6], null]) }] }]));
  assert.equal(hex(ack), hex(SyncWorld.encodeAck(11, 1)));
  const pos = w.entities.get(7).components.get(tr.id).fields[0]; assert.ok(Math.abs(pos[0] - 4) < 0.011 && Math.abs(pos[2] - 6) < 0.011);
  const rot = w.entities.get(7).components.get(tr.id).fields[1]; assert.ok(rot.slice(0, 3).every((x) => Math.abs(x) < 0.002) && Math.abs(rot[3] - 1) < 0.002, "identity quat survives quantization");
  // tick 12 lost, 13 arrives with baseline 13 (delta against 12 which we never saw) -> gap
  w.receive(delta(13, 13, [7], []));
  assert.equal(w.gaps, 1); assert.equal(w.entities.size, 0);
  assert.deepEqual(events, [["spawn", 7], ["change", 7], ["despawn", 7]]);
  assert.equal(w.receive(Uint8Array.of(0x10, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10)), null);
});

test("interpolation buffer lerps between samples and snaps on teleport", () => {
  const b = new InterpolationBuffer(lerpVec, distVec, 5);
  b.push(10, [0, 0, 0]); b.push(12, [2, 0, 0]);
  assert.deepEqual(b.sample(11), [1, 0, 0]);
  assert.deepEqual(b.sample(13), [2, 0, 0]);
  b.push(14, [100, 0, 0]);
  assert.deepEqual(b.sample(10), [100, 0, 0]);
});

test("owner ops encode as the node parses them (0xFF marker, u48 entity, u16 actor)", () => {
  const s = Schema.parse(SCHEMA); const w = new SyncWorld(s); const tr = s.byName("Transform");
  const writes = [[tr, new ComponentValue([[1, 2, 3], [0, 0, 0, 1]])]];
  assert.equal(hex(w.encodeOps([{ op: "despawn", entity: 5 }])), "ff0102000000000005");
  assert.equal(hex(w.encodeOps([{ op: "transfer", entity: 7 * 2 ** 32 + 1, to: 3 }])), "ff01030007000000010003");
  assert.equal(hex(w.encodeOps([{ op: "spawn", writes }])), "ff0101" + hex(w.encodeWrites(writes)));
  assert.equal(hex(w.encodeOps([{ op: "write", entity: 2, writes }])), "ff0100000000000002" + hex(w.encodeWrites(writes)));
});

const emptyDelta = (tick) => Uint8Array.of(0x11, 0, 0, tick >>> 8, tick & 255, 0, 0, 0, 0, 0, 0, 0, 0);
// a session over a client that records the input frames sent through it
function recordedSession(joined) {
  const frames = []; const handlers = {};
  const client = { joined, rttMs: 0, on(name, handler) { handlers[name] = handler; }, ping() {}, sendData(channel, payload) { if (payload[0] === 0x10) frames.push({ first: new DataView(payload.buffer, payload.byteOffset).getUint32(1), count: payload[5] }); } };
  const session = new SyncSession(client, "component T {\n  x: f32\n}\n", 60, 2);
  return { session, frames, delta: (tick) => handlers.data(0, 0, emptyDelta(tick)), data: (payload) => handlers.data(0, 0, payload) };
}

test("an input is resent only under the tick it was stamped with", () => {
  const { session, frames, delta } = recordedSession();
  delta(10);
  assert.equal(session.sendOps([{ op: "spawn", writes: [] }]), 12);
  assert.equal(session.sendInput(new Uint8Array([1])), 13);
  delta(14);
  assert.equal(session.sendInput(new Uint8Array([2])), 16);
  // tick 13 follows 12, so the spawn rides along; tick 16 does not follow 13, so nothing is carried over
  assert.deepEqual(frames, [{ first: 12, count: 1 }, { first: 12, count: 2 }, { first: 16, count: 1 }]);
});

test("an op sent before the first delta waits for the room's tick", () => {
  const { session, frames, delta } = recordedSession();
  session.sendOps([{ op: "spawn", writes: [] }]);
  session.sendInput(new Uint8Array([1]));
  assert.deepEqual(frames, []);
  delta(10);
  assert.deepEqual(frames, [{ first: 12, count: 1 }, { first: 12, count: 2 }]);
});

const spawnAnswer = (tag, status, entity, tick) => {
  const frame = new Uint8Array(18); const view = new DataView(frame.buffer);
  view.setUint8(0, 0x14); view.setUint32(1, tag); view.setUint8(5, status); view.setBigUint64(6, BigInt(entity)); view.setUint32(14, tick);
  return frame;
};

test("a spawn is tagged, answered once and sent again while the room is silent", () => {
  assert.throws(() => recordedSession({}).session.spawn(), /does not answer tagged spawns/);
  const { session, frames, delta, data } = recordedSession({ next_spawn_tag: 5 });
  const answers = []; session.onSpawnAnswer = (answer) => answers.push(answer);
  delta(10);
  assert.deepEqual([session.spawn(), session.spawn()], [5, 6]);
  assert.deepEqual(session.world.encodeOps([{ op: "spawn_tagged", tag: 0x01020304, writes: [] }]), Uint8Array.of(0xff, 1, 7, 1, 2, 3, 4, 0));

  data(spawnAnswer(5, 0, 0x010203040506, 12));
  data(spawnAnswer(5, 0, 0x010203040506, 12));
  assert.deepEqual(answers, [{ tag: 5, status: "created", entity: 0x010203040506, tick: 12 }]);
  data(spawnAnswer(0, 0, 9, 3));
  assert.deepEqual(answers[1], { tag: 0, status: "created", entity: 9, tick: 3 });

  // tag 6 went out for tick 13: the room is half a second past it at tick 44
  const sent = frames.length;
  delta(43);
  assert.equal(frames.length, sent);
  delta(44);
  assert.equal(frames.length, sent + 1);
  delta(45);
  assert.equal(frames.length, sent + 1);
  data(spawnAnswer(6, 1, 0, 46));
  assert.deepEqual(answers[2], { tag: 6, status: "refused", entity: 0, tick: 46 });
  delta(120);
  assert.equal(frames.length, sent + 1);
});

test("a late margin report from the room moves the next inputs further ahead", () => {
  const { session, delta, data } = recordedSession();
  delta(10);
  assert.equal(session.sendInput(new Uint8Array([1])), 12);
  data(Uint8Array.of(0x25, 0, 0, 0, 30, 0xfb, 2)); // the worst input was 5 ticks late
  delta(20);
  assert.equal(session.sendInput(new Uint8Array([2])), 28, "two ticks, plus the lateness, plus one");
});
