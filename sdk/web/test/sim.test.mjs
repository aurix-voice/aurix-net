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

test("sim wasm: navigation golden hash and crowd walk", async () => {
  const sim = await Sim.load(await readFile(WASM));
  assert.equal(sim.navGoldenHash(), 0x746c5a608abe2f26n);
  const w = sim.world({ x: 0, y: 0 }, 60);
  w.navGrid({ x: 0, y: 0 }, 1, 8, 6, [0, 1, 2, 3, 4].map((r) => [4, r]));
  const path = w.navPath({ x: 0.5, y: 0.5 }, { x: 7.5, y: 0.5 });
  assert.ok(path && path.some((p) => p.x === 4 && p.y === 5), "path should round the wall");
  assert.ok(w.navRaycast({ x: 0.5, y: 0.5 }, { x: 7.5, y: 0.5 }), "wall blocks line of sight");
  const field = w.navField({ x: 7.5, y: 0.5 });
  const a = w.addAgent(0.5, 0.5, 0.25, 3);
  w.agentFollow(a, field);
  for (let i = 0; i < 600; i++) w.crowdStep();
  const s = w.agent(a);
  assert.ok(Math.hypot(s.x - 7.5, s.y - 0.5) < 0.5, `agent at ${s.x},${s.y}`);
  w.free();
});

test("sim wasm: joints golden hash, contact events and circle cast", async () => {
  const sim = await Sim.load(await readFile(WASM));
  assert.equal(sim.jointGoldenHash(), 0x4bcc781a484ceeedn);
  const w = sim.world({ x: 0, y: -10 }, 60);
  const floor = w.addBox(BodyKind.Static, 0, -0.5, 10, 0.5);
  const ball = w.addCircle(BodyKind.Dynamic, 0, 2, 0.5);
  let begins = 0;
  for (let i = 0; i < 120; i++) {
    w.step();
    begins += w.events().filter((e) => e.begin && ((e.a === ball && e.b === floor) || (e.a === floor && e.b === ball))).length;
  }
  assert.ok(begins >= 1, "ball/floor contact begin reported");
  const hit = w.circleCast({ x: -5, y: 0.5 }, { x: 10, y: 0 }, 0.4);
  assert.ok(hit && hit.body === ball && Math.abs(hit.point.x - -0.9) < 0.01, `circle cast ${JSON.stringify(hit)}`);
  w.free();
});

test("sim wasm: 3D golden hash, resting sphere, snapshot and raycast", async () => {
  const sim = await Sim.load(await readFile(WASM));
  assert.equal(sim.golden3Hash(), 0x0a9c7e6fc4fa43bfn);
  const w = sim.world3();
  w.addBox(BodyKind.Static, { x: 0, y: -0.5, z: 0 }, { x: 10, y: 0.5, z: 10 });
  const ball = w.addSphere(BodyKind.Dynamic, { x: 0, y: 3, z: 0 }, 0.5);
  for (let i = 0; i < 240; i++) w.step();
  const b = w.body(ball);
  assert.ok(Math.abs(b.position.y - 0.5) < 0.03, `sphere y ${b.position.y}`);
  const snap = w.snapshot();
  const h = w.hash();
  w.setPosition(ball, { x: 0, y: 9, z: 0 });
  assert.notEqual(w.hash(), h);
  assert.ok(w.restore(snap));
  assert.equal(w.hash(), h);
  const hit = w.raycast({ x: 0, y: 5, z: 0 }, { x: 0, y: -10, z: 0 });
  assert.ok(hit && hit.body === ball && Math.abs(hit.point.y - 1) < 0.05, `ray ${JSON.stringify(hit)}`);
  w.free();
});
