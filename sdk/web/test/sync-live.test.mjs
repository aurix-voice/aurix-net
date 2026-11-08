// Two web clients in a sync room against a local node: NET_API_KEY=an_... NET_HTTP=http://127.0.0.1:8099 node --test test/sync-live.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { Client, SyncSession, ComponentValue } from "../dist/index.js";
import WebSocket from "ws";
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

test("sync room: owner writes replicate to a second web client", { skip: !key }, async () => {
  const a = await Client.connect(http, await grant("web-sync-a"));
  const b = await Client.connect(http, await grant("web-sync-b"));
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
  await sleep(1500); clearInterval(timer);
  assert.ok(sb.world.serverTick > 30, `b saw server ticks (${sb.world.serverTick})`);
  assert.equal(sb.world.gaps, 0, "no unrepaired gaps");
  assert.ok(seen.length >= 20, `b received ${seen.length} updates of a's avatar`);
  const last = seen[seen.length - 1];
  assert.ok(Math.abs(last[0] - x) < 3 && Math.abs(last[1] - 1) < 0.02 && Math.abs(last[2] + 2) < 0.02, `last pos ${last} vs x=${x}`);
  const own = [...sa.world.entities.values()].filter((e) => e.owner === ja.actor);
  assert.equal(own.length, 1, "a sees its own avatar");
  assert.ok(sb.renderTick > 0 && sb.renderTick <= sb.world.serverTick);
  a.close(); b.close();
});
