// Two clients over the WS tunnel against a local node: NET_API_KEY=an_... node --test test/
import { test } from "node:test";
import assert from "node:assert/strict";
import { Client } from "../dist/index.js";
import WebSocket from "ws";
if (!globalThis.WebSocket) globalThis.WebSocket = WebSocket; // Node < 22

const http = process.env.NET_HTTP ?? "http://127.0.0.1:8090";
const key = process.env.NET_API_KEY;

async function grant(player) {
  const r = await fetch(`${http}/v1/grants`, { method: "POST", headers: { authorization: `Bearer ${key}`, "content-type": "application/json" }, body: JSON.stringify({ player_id: player, name: player }) });
  return (await r.json()).grant;
}

test("relay echo over ws tunnel", { skip: !key }, async () => {
  const a = await Client.connect(http, await grant("web-a"));
  const b = await Client.connect(http, await grant("web-b"));
  const room = `web-echo-${process.pid}`;
  const ja = await a.joinOrCreate(room, "relay", 4);
  const jb = await b.joinOrCreate(room, "relay", 4);
  assert.notEqual(ja.actor, jb.actor);
  const got = []; b.on("event", (f) => got.push(f));
  a.sendEvent(1, new TextEncoder().encode("hello"), "others", true);
  for (let i = 0; i < 10; i++) { a.sendEvent(2, new Uint8Array([i]), "others", false); a.flush(); await new Promise((r) => setTimeout(r, 16)); }
  const t0 = Date.now();
  while (Date.now() - t0 < 3000 && got.filter((f) => f.code === 2).length < 10) await new Promise((r) => setTimeout(r, 20));
  const hello = got.find((f) => f.code === 1);
  assert.ok(hello && new TextDecoder().decode(hello.payload) === "hello" && hello.from === ja.actor);
  assert.equal(got.filter((f) => f.code === 2).length, 10);
  a.leave(); a.close(); b.close();
});
