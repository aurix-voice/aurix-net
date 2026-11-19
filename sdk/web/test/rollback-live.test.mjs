// Two web clients with predict/rollback in a lockstep room against a local node: inputs change
// unpredictably, both run ahead of the confirmed tick, roll back on mispredictions, and still report
// identical confirmed-state hashes (no desync). NET_API_KEY=an_... NET_HTTP=http://127.0.0.1:8098
import { test } from "node:test";
import assert from "node:assert/strict";
import { Client, LockstepClient, LockstepRollback } from "../dist/index.js";
import WebSocket from "ws";
import { key } from "./live.mjs";
if (!globalThis.WebSocket) globalThis.WebSocket = WebSocket;

const http = process.env.NET_HTTP ?? "http://127.0.0.1:8090";
const hdr = { authorization: `Bearer ${key}`, "content-type": "application/json" };
async function grant(player) {
  const r = await fetch(`${http}/v1/grants`, { method: "POST", headers: hdr, body: JSON.stringify({ player_id: player, name: player, scopes: ["join", "create"] }) });
  if (!r.ok) throw new Error(`grant failed: ${r.status} ${await r.text()} (is NET_API_KEY from this node's project?)`);
  const j = await r.json(); return j.grant ?? j.token;
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const M = 0xffffffffffffffffn;
class Toy {
  state = 0n;
  step(tick, inputs, commands) {
    for (const c of commands) this.state = (this.state * 131n + BigInt(c.actor) * 1000n + BigInt(c.data[0])) & M;
    for (const [a, i] of inputs) this.state = (this.state * 31n + BigInt(tick) * 1000n + BigInt(a) * 10n + BigInt(i[0] ?? 0)) & M; this.state = (this.state * 7n + BigInt(tick)) & M;
  }
  snapshot() { return this.state; } restore(s) { this.state = s; }
}

test("rollback: two predicting clients stay in sync on confirmed hashes while rolling back", { skip: !key }, async (t) => {
  const room = `web-rb-${process.pid}`;
  const peers = [];
  for (const name of ["rb-a", "rb-b"]) {
    const c = await Client.connect(http, await grant(name));
    t.after(() => c.close());
    const joined = await c.joinOrCreate(room, "lockstep", 2, { tick_hz: 30 });
    const ls = new LockstepClient((ch, p) => c.sendData(ch, p));
    const sim = new Toy(); const rb = new LockstepRollback(sim, joined.actor, 0, 8);
    const p = { name, c, joined, ls, sim, rb, hashes: new Map(), dumps: 0, input: 1 };
    ls.onTick = (t, inputs, commands) => { rb.confirm(t, inputs, commands); const h = rb.confirmedSnapshot(); p.hashes.set(t, h); ls.reportHash(t, h & M); };
    ls.onRestore = (t, _h, state) => { sim.state = state.length ? BigInt("0x" + Buffer.from(state).toString("hex")) : 0n; rb.reset(t); };
    ls.onDumpRequested = () => { p.dumps++; };
    c.on("data", (_ch, _tick, payload) => ls.receive(payload));
    peers.push(p);
  }
  let frame = 0;
  const timer = setInterval(() => {
    frame++;
    for (const p of peers) {
      if (frame % 7 === 0) p.input = (p.input % 5) + 1; // unpredictable-ish input changes → rollbacks
      const tick = p.ls.sendInput(Uint8Array.of(p.input)); p.rb.localInput(tick, Uint8Array.of(p.input));
      p.rb.predict(p.ls.serverTick + p.ls.inputDelay);
    }
  }, 1000 / 30);
  await sleep(6000); clearInterval(timer); await sleep(300);
  const [a, b] = peers;
  for (const p of peers) {
    assert.ok(p.rb.confirmedTick > 60, `${p.name} confirmed ${p.rb.confirmedTick}`);
    assert.ok(p.rb.predictedTicks > 0 && p.rb.headTick >= p.rb.confirmedTick, `${p.name} predicted ${p.rb.predictedTicks}`);
    assert.ok(p.rb.rollbacks > 0, `${p.name} rolled back ${p.rb.rollbacks}×`);
    assert.equal(p.dumps, 0, `${p.name} was asked for a desync dump`);
  }
  let compared = 0;
  for (const [t, h] of a.hashes) { const hb = b.hashes.get(t); if (hb === undefined) continue; compared++; assert.equal(h, hb, `confirmed state differs at tick ${t}`); }
  assert.ok(compared > 60, `compared ${compared} confirmed ticks`);
  console.log(`rollback live: a ${a.rb.rollbacks} rollbacks/${a.rb.resimulatedTicks} resim, b ${b.rb.rollbacks}/${b.rb.resimulatedTicks}, ${compared} ticks equal, delay a=${a.ls.inputDelay} b=${b.ls.inputDelay}`);
});

test("rollback: a predicting client and one that steps in onTick agree on a command", { skip: !key }, async (t) => {
  const room = `web-rb-cmd-${process.pid}`;
  const restore = (sim) => (state) => { sim.state = state.length ? BigInt("0x" + Buffer.from(state).toString("hex")) : 0n; };
  const join = async (name) => {
    const c = await Client.connect(http, await grant(name));
    t.after(() => c.close());
    const joined = await c.joinOrCreate(room, "lockstep", 2, { tick_hz: 30 });
    const ls = new LockstepClient((ch, p) => c.sendData(ch, p));
    const p = { name, joined, ls, sim: new Toy(), hashes: new Map(), commands: [], dumps: 0 };
    ls.onDumpRequested = () => { p.dumps++; };
    c.on("data", (_ch, _tick, payload) => ls.receive(payload));
    return p;
  };
  const ahead = await join("rb-ahead");
  const rb = new LockstepRollback(ahead.sim, ahead.joined.actor, 0, 8);
  ahead.ls.onTick = (tick, inputs, commands) => {
    for (const c of commands) ahead.commands.push([tick, c.actor, c.data[0]]);
    rb.confirm(tick, inputs, commands);
    const h = rb.confirmedSnapshot(); ahead.hashes.set(tick, h); ahead.ls.reportHash(tick, h & M);
  };
  ahead.ls.onRestore = (tick, _h, state) => { restore(ahead.sim)(state); rb.reset(tick); };

  const plain = await join("rb-plain");
  let pending = [];
  plain.ls.onCommand = (tick, actor, data) => { plain.commands.push([tick, actor, data[0]]); pending.push({ actor, data }); };
  plain.ls.onTick = (tick, inputs) => {
    plain.sim.step(tick, inputs, pending); pending = [];
    plain.hashes.set(tick, plain.sim.state); plain.ls.reportHash(tick, plain.sim.state & M);
  };
  plain.ls.onRestore = (_tick, _h, state) => restore(plain.sim)(state);

  let frame = 0;
  const timer = setInterval(() => {
    frame++;
    const tick = ahead.ls.sendInput(Uint8Array.of(3)); rb.localInput(tick, Uint8Array.of(3));
    rb.predict(ahead.ls.serverTick + ahead.ls.inputDelay);
    plain.ls.sendInput(Uint8Array.of(4));
    if (frame === 30) ahead.ls.sendCommand(Uint8Array.of(100));
    if (frame === 60) plain.ls.sendCommand(Uint8Array.of(50));
  }, 1000 / 30);
  await sleep(4000); clearInterval(timer); await sleep(300);

  assert.equal(ahead.commands.length, 2, `commands ${JSON.stringify(ahead.commands)}`);
  assert.deepEqual(ahead.commands, plain.commands);
  assert.ok(rb.predictedTicks > 0 && rb.rollbacks >= 2, `predicted ${rb.predictedTicks}, rolled back ${rb.rollbacks}`);
  const last = ahead.commands[1][0];
  let compared = 0, after = 0;
  for (const [tick, h] of ahead.hashes) {
    const other = plain.hashes.get(tick); if (other === undefined) continue;
    compared++; if (tick >= last) after++;
    assert.equal(h, other, `confirmed state differs at tick ${tick}`);
  }
  assert.ok(compared > 60 && after > 10, `compared ${compared} confirmed ticks, ${after} after the commands`);
  for (const p of [ahead, plain]) assert.equal(p.dumps, 0, `${p.name} was asked for a desync dump`);
  console.log(`rollback command live: commands at ticks ${ahead.commands.map(([tick]) => tick)}, ${compared} ticks equal (${after} after), ${rb.rollbacks} rollbacks`);
});
