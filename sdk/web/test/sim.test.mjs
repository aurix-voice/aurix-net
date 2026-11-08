import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { Sim, BodyKind } from "../dist/sim.js";

import { existsSync } from "node:fs";

// Built by `cargo build --release --target wasm32-unknown-unknown -p net-sim-wasm` (CI does this in the
// `rust` job); a fresh checkout without it skips these tests instead of failing on ENOENT.
const WASM = new URL("../../../target/wasm32-unknown-unknown/release/net_sim_wasm.wasm", import.meta.url);
const skip = existsSync(WASM) ? false : "net_sim_wasm.wasm not built: cargo build --release --target wasm32-unknown-unknown -p net-sim-wasm";
const GOLDEN = 0x58d9870ba45952b5n;

test("sim wasm: golden hash matches native Rust", { skip }, async () => {
  const sim = await Sim.load(await readFile(WASM));
  assert.equal(sim.goldenHash(), GOLDEN);
});

test("sim wasm: ball lands, snapshot round-trips, two worlds agree", { skip }, async () => {
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

test("sim wasm: navigation golden hash and crowd walk", { skip }, async () => {
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

test("sim wasm: joints golden hash, contact events and circle cast", { skip }, async () => {
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

test("sim wasm: 3D golden hash, resting sphere, snapshot and raycast", { skip }, async () => {
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

test("sim wasm: 3D joints golden hash, hinge door and walking character", { skip }, async () => {
  const sim = await Sim.load(await readFile(WASM));
  assert.equal(sim.joint3GoldenHash(), 0x5db56af63cceee85n);
  const w = sim.world3();
  w.addBox(BodyKind.Static, { x: 0, y: -0.5, z: 0 }, { x: 20, y: 0.5, z: 20 });
  const post = w.addBox(BodyKind.Static, { x: -10, y: 1, z: 0 }, { x: 0.1, y: 1, z: 0.1 });
  const door = w.addBox(BodyKind.Dynamic, { x: -9.4, y: 1, z: 0 }, { x: 0.5, y: 1, z: 0.05 });
  const hinge = w.jointHinge(post, door, { x: 0.1, y: 0, z: 0 }, { x: -0.5, y: 0, z: 0 }, { x: 0, y: 1, z: 0 }, { x: 0, y: 1, z: 0 }, { lower: 0, upper: Math.PI / 2 });
  w.applyImpulse(door, { x: 0, y: 0, z: 8 }, { x: -9, y: 1, z: 0 });
  w.addBox(BodyKind.Static, { x: 4, y: 0.15, z: 10 }, { x: 1, y: 0.15, z: 2 });
  const hero = w.addCapsule(BodyKind.Kinematic, { x: 0, y: 0.8, z: 10 }, 0.5, 0.3);
  const k = w.character(hero);
  for (let i = 0; i < 300; i++) {
    w.moveCharacter(k, { x: 3 / 60, y: -10 / 60, z: 0 });
    w.step();
  }
  const angle = w.hingeAngle(hinge);
  assert.ok(angle > 0.3 && angle < Math.PI / 2 + 0.1, `door angle ${angle}`);
  const p = w.body(hero).position;
  assert.ok(p.x > 5 && Math.abs(p.y - 0.8) < 0.05 && w.characterState(k).grounded, `hero ${JSON.stringify(p)}`);
  w.free();
});

test("sim wasm: 3D convex hulls golden hash, pyramid rests, sphere cast", { skip }, async () => {
  const sim = await Sim.load(await readFile(WASM));
  assert.equal(sim.hull3GoldenHash(), 0xd656fc8f55999cf8n);
  const w = sim.world3();
  w.addBox(BodyKind.Static, { x: 0, y: -0.5, z: 0 }, { x: 20, y: 0.5, z: 20 });
  const verts = [{ x: -0.5, y: -0.25, z: -0.5 }, { x: 0.5, y: -0.25, z: -0.5 }, { x: 0.5, y: -0.25, z: 0.5 }, { x: -0.5, y: -0.25, z: 0.5 }, { x: 0, y: 0.75, z: 0 }];
  const faces = [[0, 1, 2, 3], [0, 1, 4], [1, 2, 4], [2, 3, 4], [3, 0, 4]];
  const pyr = w.addConvex(BodyKind.Dynamic, { x: 0, y: 2, z: 0 }, verts, faces);
  for (let i = 0; i < 300; i++) w.step();
  const p = w.body(pyr).position;
  assert.ok(Math.abs(p.y - 0.25) < 0.06, `pyramid ${JSON.stringify(p)}`);
  const hit = w.sphereCast({ x: 0, y: 5, z: 0 }, { x: 0, y: -10, z: 0 }, 0.5);
  assert.ok(hit && hit.body === pyr && Math.abs(hit.point.y - 1.0) < 0.05 && hit.normal.y > 0.5, JSON.stringify(hit));
  const q = { x: 0, y: 0, z: 0, w: 1 };
  const bh = w.boxCast({ x: 0, y: 5, z: 0 }, q, { x: 0.5, y: 0.5, z: 0.5 }, { x: 0, y: -10, z: 0 });
  assert.ok(bh && bh.body === pyr && Math.abs(bh.point.y - 1.0) < 0.05 && Math.abs(bh.fraction - 0.35) < 0.01, `box cast ${JSON.stringify(bh)}`);
  const ch = w.capsuleCast({ x: 0, y: 5, z: 0 }, q, 0.5, 0.25, { x: 0, y: -10, z: 0 });
  assert.ok(ch && ch.body === pyr && Math.abs(ch.point.y - 1.0) < 0.05 && Math.abs(ch.fraction - 0.325) < 0.01, `capsule cast ${JSON.stringify(ch)}`);
  assert.equal(w.boxCast({ x: 0, y: 5, z: 0 }, q, { x: 0.5, y: 0.5, z: 0.5 }, { x: 10, y: 0, z: 0 }), null);
  assert.throws(() => w.addConvex(BodyKind.Static, { x: 0, y: 0, z: 0 }, verts.slice(0, 3), faces));
  w.free();
});
