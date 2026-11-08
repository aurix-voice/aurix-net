// WebRPC through a local node: NET_API_KEY=an_... NET_HTTP=http://127.0.0.1:8090 node --test test/rpc-live.test.mjs
// The node must allow loopback webhook targets (server.environment = "development").
import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { Client } from "../dist/index.js";
import WebSocket from "ws";
if (!globalThis.WebSocket) globalThis.WebSocket = WebSocket;

const base = process.env.NET_HTTP ?? "http://127.0.0.1:8090";
const key = process.env.NET_API_KEY;
const auth = { authorization: `Bearer ${key}`, "content-type": "application/json" };
async function grant(player) {
  const r = await fetch(`${base}/v1/grants`, { method: "POST", headers: auth, body: JSON.stringify({ player_id: player, name: player, scopes: ["join", "create"] }) });
  if (!r.ok) throw new Error(`grant failed: ${r.status} ${await r.text()}`);
  const j = await r.json(); return j.grant ?? j.token;
}

test("rpc: a client call reaches the project's rpc webhook signed and its JSON answer comes back", { skip: !key }, async () => {
  const seen = [];
  const srv = http.createServer((req, res) => {
    let body = ""; req.on("data", (c) => (body += c)); req.on("end", () => {
      seen.push({ headers: req.headers, body: JSON.parse(body) });
      const name = JSON.parse(body).data.name;
      if (name === "fail") { res.writeHead(409, { "content-type": "application/json" }); res.end(JSON.stringify({ reason: "nope" })); return; }
      res.writeHead(200, { "content-type": "application/json" }); res.end(JSON.stringify({ echo: JSON.parse(body).data }));
    });
  });
  await new Promise((r) => srv.listen(0, "127.0.0.1", r));
  const url = `http://127.0.0.1:${srv.address().port}/rpc`;
  const created = await fetch(`${base}/v1/webhooks`, { method: "POST", headers: auth, body: JSON.stringify({ url, events: ["rpc"] }) });
  if (created.status === 400) { srv.close(); test.skip?.("node refuses loopback webhooks (not a development node)"); return; }
  assert.ok(created.ok, `webhook create ${created.status} ${await created.clone().text()}`);
  const hook = await created.json();
  try {
    const c = await Client.connect(base, await grant("web-rpc-a"));
    const r = await c.rpc("inventory.get", { slot: 3 });
    assert.equal(r.ok, true, JSON.stringify(r));
    assert.equal(r.status, 200);
    assert.equal(r.body.echo.name, "inventory.get");
    assert.equal(r.body.echo.player_id, "web-rpc-a");
    assert.deepEqual(r.body.echo.body, { slot: 3 });
    assert.equal(seen[0].headers["x-aurix-event"], "rpc");
    assert.match(seen[0].headers["x-aurix-signature"], /^sha256=[0-9a-f]{64}$/);
    const f = await c.rpc("fail");
    assert.equal(f.ok, false); assert.equal(f.status, 409); assert.equal(f.body.reason, "nope");
    await assert.rejects(c.rpc("bad name!"), /rpc: name/);
    c.close();
  } finally {
    await fetch(`${base}/v1/webhooks/${hook.id}`, { method: "DELETE", headers: auth });
    srv.close();
  }
});
