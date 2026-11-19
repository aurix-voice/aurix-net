// Two web clients in a sync room against a local node: NET_API_KEY=an_... NET_HTTP=http://127.0.0.1:8099 node --test test/sync-live.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { Client, SyncSession, ComponentValue } from "../dist/index.js";
import WebSocket from "ws";
import { slowLink } from "./slow-link.mjs";
if (!globalThis.WebSocket) globalThis.WebSocket = WebSocket;

const http = process.env.NET_HTTP ?? "http://127.0.0.1:8090";
const key = process.env.NET_API_KEY;
const SCHEMA = "component Transform {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n}\n";
async function grant(player) {
  const r = await fetch(`${http}/v1/grants`, { method: "POST", headers: { authorization: `Bearer ${key}`, "content-type": "application/json" }, body: JSON.stringify({ player_id: player, name: player, scopes: ["join", "create"] }) });
  if (!r.ok) throw new Error(`grant failed: ${r.status} ${await r.text()} (is NET_API_KEY from this node's project?)`);
  const j = await r.json(); return j.grant ?? j.token;
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

test("sync room: owner writes replicate to a second web client", { skip: !key }, async (t) => {
  const a = await Client.connect(http, await grant("web-sync-a"));
  t.after(() => a.close());
  const b = await Client.connect(http, await grant("web-sync-b"));
  t.after(() => b.close());
  const room = `web-sync-${process.pid}`;
  const ja = await a.joinOrCreate(room, "sync", 4, { schema: SCHEMA });
  const jb = await b.joinOrCreate(room, "sync", 4, { schema: SCHEMA });
  assert.equal(ja.mode, "sync");
  const sa = new SyncSession(a, SCHEMA, 60, 2), sb = new SyncSession(b, SCHEMA, 60, 2);
  const tr = sa.world.schema.byName("Transform");
  let x = 0; const seen = [];
  sb.world.onChange = (e) => { if (e.owner === ja.actor) seen.push(sb.world.get(e, tr)?.fields[0]); };
  sb.world.onSpawn = sb.world.onChange;
  const timer = setInterval(() => { x += 0.5; sa.sendWrites([[tr, new ComponentValue([[x, 1, -2], [0, 0, 0, 1]])]]); }, 1000 / 60);
  t.after(() => clearInterval(timer));
  await sleep(1500); clearInterval(timer);
  await sleep(200); // let the last writes land before comparing
  assert.ok(sb.world.serverTick > 30, `b saw server ticks (${sb.world.serverTick})`);
  assert.equal(sb.world.gaps, 0, "no unrepaired gaps");
  assert.ok(seen.length >= 20, `b received ${seen.length} updates of a's avatar`);
  const last = seen[seen.length - 1];
  assert.ok(Math.abs(last[0] - x) < 3 && Math.abs(last[1] - 1) < 0.02 && Math.abs(last[2] + 2) < 0.02, `last pos ${last} vs x=${x}`);
  const own = [...sa.world.entities.values()].filter((e) => e.owner === ja.actor);
  assert.equal(own.length, 1, "a sees its own avatar");
  assert.ok(sb.renderTick > 0 && sb.renderTick <= sb.world.serverTick);
});

test("sync room: writes sent every frame in a slower room are not delayed", { skip: !key }, async (t) => {
  const a = await Client.connect(http, await grant("web-rate-a"));
  t.after(() => a.close());
  const b = await Client.connect(http, await grant("web-rate-b"));
  t.after(() => b.close());
  const room = `web-rate-${process.pid}`;
  const ja = await a.joinOrCreate(room, "sync", 4, { schema: SCHEMA, tick_hz: 20 });
  await b.joinOrCreate(room, "sync", 4, { schema: SCHEMA, tick_hz: 20 });
  const sa = new SyncSession(a, SCHEMA, 20, 2), sb = new SyncSession(b, SCHEMA, 20, 2);
  const tr = sa.world.schema.byName("Transform");
  let x = 0, seen = 0, behind = 0;
  sb.world.onChange = (e) => { if (e.owner === ja.actor) seen = sb.world.get(e, tr)?.fields[0][0]; };
  sb.world.onSpawn = sb.world.onChange;
  await sleep(300);
  const timer = setInterval(() => { x += 1; sa.sendWrites([[tr, new ComponentValue([[x, 1, -2], [0, 0, 0, 1]])]]); }, 1000 / 60);
  t.after(() => clearInterval(timer));
  for (let i = 0; i < 6; i++) { await sleep(500); behind = Math.max(behind, x - seen); }
  clearInterval(timer);
  await sleep(500);
  assert.ok(behind < 30, `b fell ${behind} writes (of 60 a second) behind a`);
  assert.equal(Math.round(seen), x, "the last write arrives");
});

test("sync room: writes and spawns arrive over a 200 ms round trip", { skip: !key }, async () => {
  const link = slowLink(http, 100);
  const a = await Client.connect(link.url, await grant("web-slow-a"));
  const b = await Client.connect(link.url, await grant("web-slow-b"));
  try {
    const room = `web-slow-${process.pid}`;
    const ja = await a.joinOrCreate(room, "sync", 4, { schema: SCHEMA });
    await b.joinOrCreate(room, "sync", 4, { schema: SCHEMA });
    const sa = new SyncSession(a, SCHEMA, 60, 2), sb = new SyncSession(b, SCHEMA, 60, 2);
    const tr = sa.world.schema.byName("Transform");
    const mine = () => [...sb.world.entities.values()].filter((e) => e.owner === ja.actor);
    let x = 0, seen = 0;
    sb.world.onChange = (e) => { if (e.owner === ja.actor) seen = sb.world.get(e, tr)?.fields[0][0] ?? seen; };
    sb.world.onSpawn = sb.world.onChange;
    await sleep(500);
    const timer = setInterval(() => { x += 1; sa.sendWrites([[tr, new ComponentValue([[x, 1, -2], [0, 0, 0, 1]])]]); }, 1000 / 60);
    await sleep(1000);
    sa.spawn([[tr, new ComponentValue([[9, 1, -2], [0, 0, 0, 1]])]]);
    await sleep(1000);
    clearInterval(timer);
    await sleep(700);
    assert.equal(Math.round(seen), x, "the last write arrives");
    assert.equal(mine().length, 2, "the spawned entity arrives once");
  } finally {
    a.close(); b.close(); link.close();
  }
});

test("sync room: a last write whose frame is lost still arrives", { skip: !key }, async () => {
  const link = slowLink(http, 0);
  const a = await Client.connect(link.url, await grant("web-tail-a"));
  const b = await Client.connect(http, await grant("web-tail-b"));
  try {
    const room = `web-tail-${process.pid}`;
    const ja = await a.joinOrCreate(room, "sync", 4, { schema: SCHEMA });
    await b.joinOrCreate(room, "sync", 4, { schema: SCHEMA });
    const sa = new SyncSession(a, SCHEMA, 60, 2), sb = new SyncSession(b, SCHEMA, 60, 2);
    const tr = sa.world.schema.byName("Transform");
    let seen = 0;
    sb.world.onChange = (e) => { if (e.owner === ja.actor) seen = sb.world.get(e, tr)?.fields[0][0] ?? seen; };
    sb.world.onSpawn = sb.world.onChange;
    const write = (x) => { sa.sendWrites([[tr, new ComponentValue([[x, 1, -2], [0, 0, 0, 1]])]]); a.flush(); };
    await sleep(500);
    write(1);
    await sleep(500);
    assert.equal(Math.round(seen), 1);
    link.lose(10);
    write(2);
    await sleep(500);
    assert.equal(Math.round(seen), 2, "the room got the write from a repeat of its frame");
  } finally {
    a.close(); b.close(); link.close();
  }
});

async function until(what, cond, ms = 4000) {
  const t0 = Date.now(); while (Date.now() - t0 < ms) { if (cond()) return; await sleep(20); }
  throw new Error(`timed out waiting for ${what}`);
}

test("sync room: spawned entities, ownership transfer and despawn replicate", { skip: !key }, async (t) => {
  const a = await Client.connect(http, await grant("web-own-a"));
  t.after(() => a.close());
  const b = await Client.connect(http, await grant("web-own-b"));
  t.after(() => b.close());
  const room = `web-own-${process.pid}`;
  const ja = await a.joinOrCreate(room, "sync", 4, { schema: SCHEMA });
  const jb = await b.joinOrCreate(room, "sync", 4, { schema: SCHEMA });
  const sa = new SyncSession(a, SCHEMA, 60, 2), sb = new SyncSession(b, SCHEMA, 60, 2);
  const tr = sa.world.schema.byName("Transform");
  const pose = (x) => new ComponentValue([[x, 1, -2], [0, 0, 0, 1]]);
  const mine = (w, actor) => [...w.entities.values()].filter((e) => e.owner === actor);
  await until("both avatars on b", () => mine(sb.world, ja.actor).length === 1 && mine(sb.world, jb.actor).length === 1);
  const avatarA = mine(sb.world, ja.actor)[0].id;

  // a spawns a second entity it owns; b sees it with a's ownership and the initial write
  const answers = [];
  sa.onSpawnAnswer = (answer) => answers.push(answer);
  const tag = sa.spawn([[tr, pose(9)]]);
  const keepalive = setInterval(() => sa.sendWrites([[tr, pose(0.5)]]), 1000 / 30);
  t.after(() => clearInterval(keepalive));
  await until("spawned entity on b", () => mine(sb.world, ja.actor).length === 2);
  const spawned = mine(sb.world, ja.actor).find((e) => e.id !== avatarA);
  assert.ok(Math.abs(sb.world.get(spawned, tr).fields[0][0] - 9) < 0.02, "initial write arrived with the spawn");
  // the room tells a which entity its spawn made and which one is its avatar
  await until("the answer to the spawn", () => answers.some((answer) => answer.tag === tag));
  const answer = answers.find((answer) => answer.tag === tag);
  assert.deepEqual([answer.status, answer.entity], ["created", spawned.id]);
  assert.equal(a.avatar, avatarA);

  // a hands it to b; b now owns two entities and may write it, a sees the new value
  sa.transfer(spawned.id, jb.actor);
  await until("owner change on b", () => sb.world.entities.get(spawned.id)?.owner === jb.actor);
  await until("owner change on a", () => sa.world.entities.get(spawned.id)?.owner === jb.actor);
  const bw = setInterval(() => sb.writeEntity(spawned.id, [[tr, pose(21)]]), 1000 / 30);
  t.after(() => clearInterval(bw));
  await until("b's write visible on a", () => Math.abs((sa.world.get(sa.world.entities.get(spawned.id), tr)?.fields[0][0] ?? 0) - 21) < 0.02);
  clearInterval(bw);

  // a may no longer write it; b despawns it and both sides forget it
  sa.writeEntity(spawned.id, [[tr, pose(33)]]);
  await sleep(300);
  assert.ok(Math.abs(sb.world.get(sb.world.entities.get(spawned.id), tr).fields[0][0] - 21) < 0.02, "non-owner write ignored");
  sb.despawn(spawned.id);
  const bk = setInterval(() => sb.sendWrites([[tr, pose(1)]]), 1000 / 30);
  t.after(() => clearInterval(bk));
  await until("despawn on a", () => !sa.world.entities.has(spawned.id));
  await until("despawn on b", () => !sb.world.entities.has(spawned.id));
  clearInterval(bk); clearInterval(keepalive);
  assert.equal(sa.world.gaps + sb.world.gaps, 0, "no unrepaired gaps");
});

test("the round-trip probes of a sync session stay out of the control events", { skip: !key }, async () => {
  const a = await Client.connect(http, await grant("web-probe-a"));
  try {
    const pongs = [];
    a.on("control", (message) => { if (message.t === "pong") pongs.push(message.ts); });
    await a.joinOrCreate(`web-probe-${process.pid}`, "sync", 4, { schema: SCHEMA });
    new SyncSession(a, SCHEMA, 60, 2);
    a.send({ t: "ping", ts: 987654321 });
    await sleep(2500);
    assert.deepEqual(pongs, [987654321], "only the game's own ping is answered to the game");
  } finally {
    a.close();
  }
});
