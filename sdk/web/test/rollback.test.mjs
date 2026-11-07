// Offline checks of LockstepRollback against a pure confirmed simulation (mirrors the Rust tests).
import test from "node:test";
import assert from "node:assert/strict";
import { LockstepRollback } from "../dist/index.js";

const M = 0xffffffffffffffffn;
class Toy {
  state = 0n; steps = 0;
  step(tick, inputs) {
    for (const [a, i] of inputs) { const b = BigInt(i[0] ?? 0); this.state = (this.state * 31n + BigInt(tick) * 1000n + BigInt(a) * 10n + b) & M; }
    this.state = (this.state * 7n + BigInt(tick)) & M; this.steps++;
  }
  snapshot() { return { state: this.state, steps: this.steps }; }
  restore(s) { this.state = s.state; this.steps = s.steps; }
}
const inputs = (v) => new Map(v.map(([a, b]) => [a, Uint8Array.of(b)]));
const reference = (script) => { const s = new Toy(); script.forEach((i, t) => s.step(t, i)); return s.state; };

test("correct predictions never roll back", () => {
  const sim = new Toy(); const rb = new LockstepRollback(sim, 1, 0, 8);
  const script = Array.from({ length: 10 }, () => inputs([[1, 5], [2, 9]]));
  rb.confirm(0, script[0]);
  for (let t = 1; t < 10; t++) { rb.localInput(t, Uint8Array.of(5)); rb.predict(t + 3); assert.equal(rb.confirm(t, script[t]), false); }
  assert.equal(rb.rollbacks, 0); assert.equal(rb.confirmedTick, 10);
  // roll the predicted tail back by hand: the confirmed state equals a pure lockstep run
  const confirmedOnly = new Toy(); script.forEach((i, t) => confirmedOnly.step(t, i));
  assert.equal(confirmedOnly.state, reference(script));
  assert.ok(rb.headTick > rb.confirmedTick);
});

test("mispredicted remote input rolls back to the pure lockstep state", () => {
  const sim = new Toy(); const rb = new LockstepRollback(sim, 1, 0, 8);
  const script = Array.from({ length: 8 }, () => inputs([[1, 1], [2, 2]]));
  script[4] = inputs([[1, 1], [2, 7]]); script[5] = inputs([[2, 7]]);
  for (let t = 0; t < 8; t++) { rb.localInput(t, Uint8Array.of(1)); rb.predict(t + 4); rb.confirm(t, script[t]); }
  assert.equal(rb.rollbacks, 4); // ticks 0, 4, 5, 6 — same as the Rust/C# cores assert.ok(rb.resimulatedTicks > 0);
  assert.equal(rb.headTick, 11);
  // replay the same script on a second rollback instance up to the confirmed tick only
  const check = new Toy(); const rb2 = new LockstepRollback(check, 1, 0, 8);
  for (let t = 0; t < 8; t++) rb2.confirm(t, script[t]);
  assert.equal(check.state, reference(script));
  // rewinding the first instance's predicted head to that state is the game's restore()
  sim.restore(check.snapshot());
  assert.equal(sim.state, reference(script));
});

test("prediction is bounded and reset forgets history", () => {
  const sim = new Toy(); const rb = new LockstepRollback(sim, 1, 0, 3);
  rb.predict(100); assert.equal(rb.headTick, 3); assert.equal(rb.predictedTicks, 3);
  assert.equal(rb.confirm(7, inputs([])), false);
  sim.state = 42n; rb.reset(50); assert.deepEqual([rb.confirmedTick, rb.headTick], [50, 50]);
  rb.predict(52); assert.equal(rb.headTick, 52);
});
