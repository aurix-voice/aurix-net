// Three web clients in a lockstep room against a local node; one lies about its hash so the node
// collects state dumps. NET_API_KEY=an_... NET_HTTP=http://127.0.0.1:8099 node --test test/lockstep-live.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { Client, LockstepClient, Channel, MAX_COMMAND } from "../dist/index.js";
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
const tickBytes = (t) => Uint8Array.of(t >>> 24, (t >>> 16) & 255, (t >>> 8) & 255, t & 255);

test("lockstep: confirmed ticks flow, margin reports arrive, a liar's desync yields stored dumps", { skip: !key }, async (t) => {
  const room = `web-ls-${process.pid}`;
  const peers = [];
  for (const name of ["ls-a", "ls-b", "ls-c"]) {
    const c = await Client.connect(http, await grant(name));
    t.after(() => c.close());
    const joined = await c.joinOrCreate(room, "lockstep", 4, { tick_hz: 30 });
    const ls = new LockstepClient((ch, p) => c.sendData(ch, p));
    const p = { name, c, joined, ls, ticks: 0, stats: 0, dumpsAsked: [], liar: name === "ls-c" };
    const orig = ls.onInputStats.bind(ls); ls.onInputStats = (...report) => { p.stats++; orig(...report); };
    ls.onTick = (t) => { p.ticks++; if (t % 10 === 0) ls.reportHash(t, p.liar && t > 60 ? 0xbadn : LockstepClient.fnv1a(tickBytes(t))); };
    ls.onDumpRequested = (t) => { p.dumpsAsked.push(t); ls.sendDump(t, LockstepClient.fnv1a(tickBytes(t)), new TextEncoder().encode(`${name}@${t}`)); };
    c.on("data", (_ch, _tick, payload) => ls.receive(payload));
    peers.push(p);
  }
  const timer = setInterval(() => { for (const p of peers) p.ls.sendInput(Uint8Array.of(p.name.charCodeAt(3))); }, 1000 / 30);
  await sleep(5000); clearInterval(timer);
  for (const p of peers) assert.ok(Number.isSafeInteger(p.joined.seed) && p.joined.seed === peers[0].joined.seed, `${p.name} joined with seed ${p.joined.seed}`);
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
});

test("lockstep: a tick's commands come with their senders, before the tick and apart from the players' inputs", { skip: !key }, async (t) => {
  const room = `web-ls-apart-${process.pid}`;
  const peers = [];
  for (const name of ["ls-ca", "ls-cb"]) {
    const c = await Client.connect(http, await grant(name));
    t.after(() => c.close());
    const joined = await c.joinOrCreate(room, "lockstep", 4, { tick_hz: 30 });
    const ls = new LockstepClient((ch, p) => c.sendData(ch, p));
    const p = { c, ls, actor: joined.actor, heard: [], stepped: -1, late: 0, asPlayer: 0 };
    ls.onCommand = (tick, actor, data) => { p.heard.push([tick, actor, data[0]]); if (tick <= p.stepped) p.late++; };
    ls.onTick = (tick, inputs) => { p.stepped = tick; if (inputs.has(65535)) p.asPlayer++; };
    c.on("data", (_ch, _tick, payload) => ls.receive(payload));
    peers.push(p);
  }
  const [a, b] = peers;
  const timer = setInterval(() => { for (const p of peers) p.ls.sendInput(Uint8Array.of(1)); }, 1000 / 30);
  t.after(() => clearInterval(timer));
  await sleep(300);
  a.ls.sendCommand(Uint8Array.of(1));
  a.ls.sendCommand(Uint8Array.of(2));
  b.ls.sendCommand(Uint8Array.of(3));
  for (let i = 0; i < 100 && (a.heard.length < 3 || b.heard.length < 3); i++) await sleep(20);

  assert.equal(a.heard.length, 3, JSON.stringify(a.heard));
  assert.deepEqual(b.heard, a.heard, "both players get the commands at the same ticks, in the same order");
  const own = a.heard.filter(([, actor]) => actor === a.actor).map(([, , byte]) => byte);
  assert.deepEqual(own, [1, 2], "a player's commands keep the order they were sent in");
  assert.deepEqual(a.heard.filter(([, actor]) => actor === b.actor).map(([, , byte]) => byte), [3]);
  assert.equal(a.late + b.late, 0, "a command comes before the tick it belongs to");
  assert.equal(a.asPlayer + b.asPlayer, 0, "the commands are not among the players' inputs");
});

test("lockstep: a command the node refuses comes back as a control error", { skip: !key }, async (t) => {
  const c = await Client.connect(http, await grant("ls-cmd"));
  t.after(() => c.close());
  await c.joinOrCreate(`web-ls-cmd-${process.pid}`, "lockstep", 4, { tick_hz: 30 });
  const ls = new LockstepClient((ch, p) => c.sendData(ch, p));
  const errors = [], commands = [];
  ls.onCommand = (tick, _actor, data) => commands.push([tick, data[0]]);
  c.on("data", (_ch, _tick, payload) => ls.receive(payload));
  c.on("control", (message) => { if (message.t === "error") errors.push(message); });
  const timer = setInterval(() => ls.sendInput(Uint8Array.of(1)), 1000 / 30);
  t.after(() => clearInterval(timer));
  await sleep(300);

  // past the sender's own check, the way an older client would send it
  const tooLong = new Uint8Array(2 + MAX_COMMAND);
  tooLong[0] = 0x28;
  c.sendData(Channel.Reliable, tooLong);
  for (let i = 0; i < 100 && !errors.length; i++) await sleep(20);
  assert.deepEqual(errors.splice(0), [{ t: "error", code: "invalid_request", message: `invalid request: a lockstep command takes 1 to ${MAX_COMMAND} bytes, got ${MAX_COMMAND + 1}` }]);

  // 40 commands of 200 bytes in one go: a tick carries one, 4096 bytes may wait, the rest is refused
  for (let i = 0; i < 40; i++) ls.sendCommand(new Uint8Array(200).fill(i));
  await sleep(1500);
  assert.ok(errors.length > 0 && errors.every((error) => error.code === "rate_limited" && error.message === "rate limited"), JSON.stringify(errors.slice(0, 3)));
  assert.equal(commands.length + errors.length, 40);
  assert.equal(new Set(commands.map(([tick]) => tick)).size, commands.length);
  assert.deepEqual(commands.map(([, first]) => first).slice(0, 20), Array.from({ length: 20 }, (_, i) => i));
});
