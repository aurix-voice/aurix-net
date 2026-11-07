// Live rehost check (not part of `npm test`): needs two nodes (scripts/two-nodes.sh) and NET_API_KEY.
// Joins a relay room on node A, waits for `rehost` (A got SIGTERM), rejoins via node B with the
// same grant, then a second client on B must still reach it.  Usage: node test/rehost.mjs
import { Client } from "../dist/index.js";
import WebSocket from "ws";
if (!globalThis.WebSocket) globalThis.WebSocket = WebSocket;
const A = "http://127.0.0.1:8090", B = "http://127.0.0.1:8091", key = process.env.NET_API_KEY;
async function grant(http, player) {
  const r = await fetch(`${http}/v1/grants`, { method: "POST", headers: { authorization: `Bearer ${key}`, "content-type": "application/json" }, body: JSON.stringify({ player_id: player, name: player }) });
  return (await r.json()).grant;
}
const room = process.argv[2] ?? `web-rehost-${process.pid}`;
const a = await Client.connect(A, await grant(A, "web-rehost-a"));
const j1 = await a.joinOrCreate(room, "relay", 8);
console.log(`web: joined ${room} actor=${j1.actor} on A`);
let frames = 0; a.on("data", () => frames++);
const rehost = new Promise((res) => a.on("rehost", res));
const timer = setInterval(() => { a.sendEvent(1, new Uint8Array([1]), "others", false); a.flush(); }, 50);
await Promise.race([rehost, new Promise((_, rej) => setTimeout(() => rej(new Error("no rehost within 60 s")), 60000))]);
clearInterval(timer);
console.log(`web: got rehost after ${frames} data frames`);
const { client: a2, joined: j2 } = await a.rejoin([B]);
console.log(`web: rejoined actor=${j2.actor} (same actor: ${j2.actor === j1.actor})`);
const got = []; a2.on("event", (f) => got.push(f));
const b = await Client.connect(B, await grant(B, "web-rehost-b"));
await b.join({ t: "join", name: room });
b.sendEvent(7, new TextEncoder().encode("after-rehost"), "others", true); b.flush();
const t0 = Date.now(); while (Date.now() - t0 < 3000 && !got.some((f) => f.code === 7)) await new Promise((r) => setTimeout(r, 20));
const ok = got.some((f) => f.code === 7 && new TextDecoder().decode(f.payload) === "after-rehost");
console.log(ok && j2.actor === j1.actor ? "WEB_REHOST_OK" : `WEB_REHOST_FAIL events=${got.length}`);
a2.close(); b.close(); process.exit(ok ? 0 : 1);
