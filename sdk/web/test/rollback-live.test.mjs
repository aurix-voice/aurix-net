// Two web clients with predict/rollback in a lockstep room against a local node: inputs change
// unpredictably, both run ahead of the confirmed tick, roll back on mispredictions, and still report
// identical confirmed-state hashes (no desync). NET_API_KEY=an_... NET_HTTP=http://127.0.0.1:8098
import { test } from "node:test";
import assert from "node:assert/strict";
import { Client, LockstepClient, LockstepRollback } from "../dist/index.js";
import WebSocket from "ws";
if (!globalThis.WebSocket) globalThis.WebSocket = WebSocket;

const http = process.env.NET_HTTP ?? "http://127.0.0.1:8090";
const key = process.env.NET_API_KEY;
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
  step(tick, inputs) { for (const [a, i] of inputs) this.state = (this.state * 31n + BigInt(tick) * 1000n + BigInt(a) * 10n + BigInt(i[0] ?? 0)) & M; this.state = (this.state * 7n + BigInt(tick)) & M; }
  snapshot() { return this.state; } restore(s) { this.state = s; }
}

test("rollback: two predicting clients stay in sync on confirmed hashes while rolling back", { skip: !key }, async () => {
  const room = `web-rb-${process.pid}`;
  const peers = [];
  for (const name of ["rb-a", "rb-b"]) {
    const c = await Client.connect(http, await grant(name));
    const joined = await c.joinOrCreate(room, "lockstep", 2, { tick_hz: 30 });
    const ls = new LockstepClient((ch, p) => c.sendData(ch, p));
    const sim = new Toy(); const rb = new LockstepRollback(sim, joined.actor, 0, 8);
    const p = { name, c, joined, ls, sim, rb, hashes: new Map(), dumps: 0, input: 1 };
    ls.onTick = (t, inputs) => { rb.confirm(t, inputs); const h = rb.confirmedSnapshot(); p.hashes.set(t, h); ls.reportHash(t, h & M); };
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
  for (const p of peers) p.c.close();
});
