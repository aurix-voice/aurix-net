import { test } from "node:test";
import assert from "node:assert/strict";
import { LockstepClient, Channel } from "../dist/index.js";
const hex = (b) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
const bundle = (tick, inputs) => { const parts = []; for (const [a, d] of inputs) parts.push(a >> 8, a & 255, d.length, ...d); const one = [0x20, tick >>> 24, (tick >>> 16) & 255, (tick >>> 8) & 255, tick & 255, inputs.length, ...parts]; return one; };
const datagram = (...bundles) => { const out = [0x20, bundles.length]; for (const b of bundles) out.push(b.length >> 8, b.length & 255, ...b); return Uint8Array.from(out); };

test("bundles are delivered once, in tick order, with redundancy and gaps filled later", () => {
  const sent = []; const ls = new LockstepClient((ch, p) => sent.push([ch, p]));
  const ticks = []; ls.onTick = (t, inputs) => ticks.push([t, [...inputs.keys()]]);
  ls.receive(datagram(bundle(5, [[1, [9]]])));
  ls.receive(datagram(bundle(5, [[1, [9]]]), bundle(6, [[1, [8]], [2, [7]]])));
  ls.receive(datagram(bundle(8, [])));                     // 7 missing
  assert.deepEqual(ticks, [[5, [1]], [6, [1, 2]]]);
  ls.receive(datagram(bundle(6, []), bundle(7, [[2, [1]]]), bundle(8, [])));
  assert.deepEqual(ticks.map((t) => t[0]), [5, 6, 7, 8]);
  assert.equal(ls.serverTick, 9); assert.equal(ls.confirmedTick, 9); assert.equal(ls.delivered, 4);
});

test("catch-up restores then continues; hashes/checkpoints/dumps are framed like the managed core", () => {
  const sent = []; const ls = new LockstepClient((ch, p) => sent.push([ch, p]));
  let restored; ls.onRestore = (t, h, s) => (restored = [t, h, hex(s)]);
  const state = Uint8Array.of(1, 2, 3);
  ls.receive(Uint8Array.from([0x24, 0, 0, 1, 44, 0, 0, 0, 0, 0, 0, 0, 0x2a, 0, 0, 0, 3, ...state]));
  assert.deepEqual(restored, [300, 42n, "010203"]); assert.equal(ls.confirmedTick, 300); assert.equal(ls.catchups, 1);
  ls.reportHash(300, 0x1122334455667788n);
  assert.equal(hex(sent.at(-1)[1]), "210000012c1122334455667788"); assert.equal(sent.at(-1)[0], Channel.Unreliable);
  let asked; ls.onDumpRequested = (t) => (asked = t);
  ls.receive(Uint8Array.of(0x26, 0, 0, 1, 0)); assert.equal(asked, 256);
  ls.sendDump(256, 0x1122334455667788n, Uint8Array.of(7, 7));
  assert.equal(hex(sent.at(-1)[1]), "270000010011223344556677880707"); assert.equal(sent.at(-1)[0], Channel.Reliable);
  let ck; ls.onCheckpointRequested = (t) => (ck = t); ls.receive(Uint8Array.of(0x23, 0, 0, 0, 9)); assert.equal(ck, 9);
  assert.equal(LockstepClient.fnv1a(new TextEncoder().encode("a")), 0xaf63dc4c8601ec8cn);
});

test("adaptive input delay follows the node's margin reports; redundancy window is configurable", () => {
  const sent = []; const ls = new LockstepClient((ch, p) => sent.push([ch, p]));
  ls.onInputStats(-2, 2); assert.equal(ls.inputDelay, 4); assert.equal(ls.lateInputs, 2);
  ls.onInputStats(3, 0); ls.onInputStats(3, 0); assert.equal(ls.inputDelay, 4);
  ls.onInputStats(3, 0); assert.equal(ls.inputDelay, 3);
  ls.onInputStats(1, 0); ls.onInputStats(3, 0); ls.onInputStats(3, 0); ls.onInputStats(3, 0); assert.equal(ls.inputDelay, 2);
  ls.inputDelay = ls.inputDelayMax; ls.onInputStats(-9, 1); assert.equal(ls.inputDelay, ls.inputDelayMax);
  assert.ok(ls.receive(Uint8Array.of(0x25, 0, 0, 0, 10, 0xff, 1))); assert.equal(ls.lastMargin, -1);
  ls.inputRedundancy = 2; ls.sendInput(Uint8Array.of(1)); ls.sendInput(Uint8Array.of(2)); const t = ls.sendInput(Uint8Array.of(3));
  const last = sent.at(-1)[1]; assert.equal(last[0], 0x10); assert.equal(last[5], 2); assert.equal(((last[1] << 24) | (last[2] << 16) | (last[3] << 8) | last[4]) >>> 0, t - 1);
});
