import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { Sim, BodyKind } from "../dist/sim.js";

const WASM = new URL("../../../target/wasm32-unknown-unknown/release/net_sim_wasm.wasm", import.meta.url);
const GOLDEN = 0x58d9870ba45952b5n;

test("sim wasm: golden hash matches native Rust", async () => {
  const sim = await Sim.load(await readFile(WASM));
  assert.equal(sim.goldenHash(), GOLDEN);
});

test("sim wasm: ball lands, snapshot round-trips, two worlds agree", async () => {
  const sim = await Sim.load(await readFile(WASM));
  const build = () => {
    const w = sim.world();
    w.addBox(BodyKind.Static, 0, -1, 50, 1);
    const ball = w.addCircle(BodyKind.Dynamic, 0, 5, 1);
    w.setMaterial(ball, 0.3, 0.2);
    return { w, ball };
  };
  const a = build();
  const b = build();
  for (let i = 0; i < 120; i++) {
    a.w.step();
    b.w.step();
  }
  assert.equal(a.w.hash(), b.w.hash());
  const y = a.w.body(a.ball).y;
  assert.ok(y > 0.9 && y < 1.1, `ball at ${y}`);
  const snap = a.w.snapshot();
  const h = a.w.hash();
  a.w.step();
  assert.ok(a.w.restore(snap));
  assert.equal(a.w.hash(), h);
  const hit = a.w.raycast(0, 5, 0, -10);
  assert.equal(hit?.body, a.ball);
  assert.ok(Math.abs(hit.normal.y - 1) < 1e-6);
  a.w.free();
  b.w.free();
});
