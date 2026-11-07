import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { BehaviorTree, StateMachine, Blackboard } from "../dist/bots.js";

const vectors = JSON.parse(await readFile(new URL("../../../crates/net-sim-bots/fixtures/vectors.json", import.meta.url), "utf8"));
const C_SEE = 1, A_PATROL = 10, A_CHASE = 11, A_LOG = 12;

function scripted() {
  const b = { t: 0, patrolLeft: 2 };
  b.condition = (id, bb) => id === C_SEE ? (b.t >= 8 && b.t <= 12) || (b.t >= 20 && b.t <= 22) : id === 2 ? bb.get(50) === 1n : false;
  b.action = (id, bb) => {
    if (id === A_PATROL) { if (b.patrolLeft === 0) { b.patrolLeft = 3; return "Success"; } b.patrolLeft--; return "Running"; }
    if (id === A_CHASE) { bb.set(100, bb.get(100) + 1n); return "Running"; }
    if (id === A_LOG) { bb.set(101, bb.get(101) + 1n); return "Success"; }
    if (id === 20) return "Success";
    if (id === 30) return "Running";
    if (id >= 100 && id <= 103) { bb.set(id, bb.get(id) + 1n); return "Success"; }
    return "Failure";
  };
  b.abort = (_id, bb) => bb.set(102, bb.get(102) + 1n);
  return b;
}
const bbJson = (bb) => Object.fromEntries(bb.entries().map(([k, v]) => [String(k), Number(v)]));

test("bots: behavior tree replays the Rust vectors (status, hash, blackboard)", () => {
  const tree = BehaviorTree.fromJSON(vectors.tree);
  const bb = new Blackboard();
  const brain = scripted();
  vectors.tree_ticks.forEach((exp, t) => {
    brain.t = t;
    const st = tree.step(brain, bb);
    assert.equal(st, exp.status, `tick ${t} status`);
    assert.equal("0x" + tree.hash().toString(16), exp.hash, `tick ${t} hash`);
    assert.deepEqual(bbJson(bb), exp.bb, `tick ${t} blackboard`);
  });
});

test("bots: state machine replays the Rust vectors", () => {
  const fsm = StateMachine.fromJSON(vectors.fsm);
  const bb = new Blackboard();
  const brain = scripted();
  brain.patrolLeft = 0;
  fsm.start(brain, bb);
  vectors.fsm_ticks.forEach((exp, t) => {
    brain.t = t;
    if (t === 25) bb.set(50, 1);
    fsm.step(brain, bb);
    assert.equal(fsm.current, exp.current, `tick ${t} state`);
    assert.equal("0x" + fsm.hash().toString(16), exp.hash, `tick ${t} hash`);
    assert.deepEqual(bbJson(bb), exp.bb, `tick ${t} blackboard`);
  });
});
