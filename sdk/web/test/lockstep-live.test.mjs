// Three web clients in a lockstep room against a local node; one lies about its hash so the node
// collects state dumps. NET_API_KEY=an_... NET_HTTP=http://127.0.0.1:8099 node --test test/lockstep-live.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { Client, LockstepClient, Channel } from "../dist/index.js";
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
const tickBytes = (t) => Uint8Array.of(t >>> 24, (t >>> 16) & 255, (t >>> 8) & 255, t & 255);

test("lockstep: confirmed ticks flow, margin reports arrive, a liar's desync yields stored dumps", { skip: !key }, async () => {
  const room = `web-ls-${process.pid}`;
  const peers = [];
  for (const name of ["ls-a", "ls-b", "ls-c"]) {
    const c = await Client.connect(http, await grant(name));
    const joined = await c.joinOrCreate(room, "lockstep", 4, { tick_hz: 30 });
    const ls = new LockstepClient((ch, p) => c.sendData(ch, p));
    const p = { name, c, joined, ls, ticks: 0, stats: 0, dumpsAsked: [], liar: name === "ls-c" };
    const orig = ls.onInputStats.bind(ls); ls.onInputStats = (m, l) => { p.stats++; orig(m, l); };
    ls.onTick = (t) => { p.ticks++; if (t % 10 === 0) ls.reportHash(t, p.liar && t > 60 ? 0xbadn : LockstepClient.fnv1a(tickBytes(t))); };
    ls.onDumpRequested = (t) => { p.dumpsAsked.push(t); ls.sendDump(t, LockstepClient.fnv1a(tickBytes(t)), new TextEncoder().encode(`${name}@${t}`)); };
    c.on("data", (_ch, _tick, payload) => ls.receive(payload));
    peers.push(p);
  }
  const timer = setInterval(() => { for (const p of peers) p.ls.sendInput(Uint8Array.of(p.name.charCodeAt(3))); }, 1000 / 30);
  await sleep(5000); clearInterval(timer);
  for (const p of peers) assert.ok(p.ticks > 60, `${p.name} delivered ${p.ticks} ticks`);
  for (const p of peers) assert.ok(p.stats >= 1, `${p.name} got ${p.stats} input-stat reports (delay now ${p.ls.inputDelay}, margin ${p.ls.lastMargin})`);
  const liar = peers[2];
  assert.ok(liar.dumpsAsked.length >= 1, `liar was asked for ${liar.dumpsAsked.length} dumps`);
  await sleep(500);
  const roomId = liar.joined.room;
  const list = await (await fetch(`${http}/v1/rooms/${roomId}/desyncs`, { headers: hdr })).json();
  const actors = new Set(list.desyncs.map((d) => d.actor));
  assert.ok(list.desyncs.length >= 2, `dumps stored: ${JSON.stringify(list.desyncs)}`);
  assert.ok(actors.has(liar.joined.actor), "liar's dump stored");
  assert.ok(actors.size >= 2, "a second (reference) dump stored");
  const d = list.desyncs.find((x) => x.actor === liar.joined.actor);
  const blob = await fetch(`${http}/v1/rooms/${roomId}/desyncs/${d.tick}/${d.actor}`, { headers: hdr });
  assert.equal(blob.headers.get("content-type"), "application/octet-stream");
  assert.equal(await blob.text(), `ls-c@${d.tick}`);
  for (const p of peers) p.c.close();
});
