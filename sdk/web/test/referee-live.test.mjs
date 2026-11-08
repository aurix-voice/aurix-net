// Lockstep room with a `logic` module: the node runs the sim itself (referee). One client lies
// about its hash; the node must store its OWN state as the reference dump (actor 0) and the room
// detail must report `referee: true`. NET_API_KEY=an_... NET_HTTP=http://127.0.0.1:8099 node --test test/referee-live.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { Client, LockstepClient } from "../dist/index.js";
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

const logic = process.env.NET_LOGIC ?? "arena";
// The module has to be uploaded to the key's project first (`PUT /v1/modules/:name`, docs/modes.md); skip, don't fail, otherwise.
const hasModule = key
  ? await fetch(`${http}/v1/modules`, { headers: hdr }).then((r) => (r.ok ? r.json() : { modules: [] })).then((j) => (j.modules ?? []).some((m) => m.name === logic))
  : false;

test("lockstep referee: the node's own state is the reference dump (actor 0)", { skip: !key ? true : !hasModule ? `game module '${logic}' not uploaded to this project` : false }, async () => {
  const room = `web-ref-${process.pid}`;
  const peers = [];
  for (const name of ["rf-a", "rf-b"]) {
    const c = await Client.connect(http, await grant(name));
    const joined = await c.joinOrCreate(room, "lockstep", 4, { tick_hz: 30, logic, ...JSON.parse(process.env.NET_LOGIC_PROPS ?? "{}") });
    const ls = new LockstepClient((ch, p) => c.sendData(ch, p));
    const p = { name, c, joined, ls, ticks: 0, dumpsAsked: [], liar: name === "rf-b" };
    // honest clients do not run the sim here, so only the liar reports (a wrong) hash
    ls.onTick = (t) => { p.ticks++; if (p.liar && t % 10 === 0 && t > 60) ls.reportHash(t, 0xbadn); };
    ls.onDumpRequested = (t) => { p.dumpsAsked.push(t); ls.sendDump(t, 0xbadn, new TextEncoder().encode(`${name}@${t}`)); };
    c.on("data", (_ch, _tick, payload) => ls.receive(payload));
    peers.push(p);
  }
  const timer = setInterval(() => { for (const p of peers) p.ls.sendInput(Uint8Array.of(127, 0, 0)); }, 1000 / 30);
  await sleep(5000); clearInterval(timer);
  for (const p of peers) assert.ok(p.ticks > 60, `${p.name} delivered ${p.ticks} ticks`);
  const liar = peers[1];
  assert.ok(liar.dumpsAsked.length >= 1, `liar was asked for ${liar.dumpsAsked.length} dumps`);
  await sleep(500);
  const roomId = liar.joined.room;
  const detail = await (await fetch(`${http}/v1/rooms/${roomId}`, { headers: hdr })).json();
  assert.equal(detail.referee, true, `room detail ${JSON.stringify(detail).slice(0, 200)}`);
  const list = await (await fetch(`${http}/v1/rooms/${roomId}/desyncs`, { headers: hdr })).json();
  const actors = new Set(list.desyncs.map((d) => d.actor));
  assert.ok(actors.has(liar.joined.actor), `liar's dump stored: ${JSON.stringify(list.desyncs)}`);
  assert.ok(actors.has(0), `referee dump (actor 0) stored: ${JSON.stringify(list.desyncs)}`);
  assert.ok(!actors.has(peers[0].joined.actor), "the honest master was not asked for a dump");
  const ref = list.desyncs.find((x) => x.actor === 0);
  const sameTick = list.desyncs.find((x) => x.actor === liar.joined.actor && x.tick === ref.tick);
  assert.ok(sameTick, "referee and liar dumps share a tick");
  const blob = await fetch(`${http}/v1/rooms/${roomId}/desyncs/${ref.tick}/0`, { headers: hdr });
  assert.equal(blob.headers.get("content-type"), "application/octet-stream");
  const bytes = new Uint8Array(await blob.arrayBuffer());
  assert.ok(bytes.length > 0 && ref.bytes === bytes.length, `referee state ${bytes.length} bytes`);
  for (const p of peers) p.c.close();
});
