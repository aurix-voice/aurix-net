// A client that sends nothing outlives the node's control timeout: NET_API_KEY=an_... node --test test/
import { test } from "node:test";
import assert from "node:assert/strict";
import { Client } from "../dist/index.js";
import WebSocket from "ws";
import { key } from "./live.mjs";
if (!globalThis.WebSocket) globalThis.WebSocket = WebSocket; // Node < 22

const http = process.env.NET_HTTP ?? "http://127.0.0.1:8090";

async function grant(player) {
  const r = await fetch(`${http}/v1/grants`, { method: "POST", headers: { authorization: `Bearer ${key}`, "content-type": "application/json" }, body: JSON.stringify({ player_id: player, name: player }) });
  if (!r.ok) throw new Error(`grant failed: ${r.status} ${await r.text()} (is NET_API_KEY from this node's project?)`);
  return (await r.json()).grant;
}

// the node closes a control channel it has heard nothing from for 90 s
test("an idle client stays connected", { skip: !key }, async (t) => {
  const outside = await Client.connect(http, await grant("web-idle-a"));
  t.after(() => outside.close());
  const inside = await Client.connect(http, await grant("web-idle-b"));
  t.after(() => inside.close());
  const closed = [];
  outside.on("closed", (reason) => closed.push(`outside a room: ${reason}`));
  inside.on("closed", (reason) => closed.push(`in a room: ${reason}`));
  await inside.joinOrCreate(`web-idle-${process.pid}`, "relay", 4);
  await new Promise((done) => setTimeout(done, 95000));
  assert.deepEqual(closed, []);
  assert.ok(outside.rttMs > 0 || inside.rttMs > 0, "no probe was answered");
  assert.ok((await outside.lobbyStats()).rooms >= 1);
});
