// Offline checks of the sync codec against golden vectors produced by the managed C# core
// (sdk/unity/BuildCheck~/managed-tests prints them as `VECTOR ...`): node --test test/
import { test } from "node:test";
import assert from "node:assert/strict";
import { BitWriter, BitReader, Schema, ComponentValue, SyncWorld, InterpolationBuffer, lerpVec, distVec } from "../dist/index.js";

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
