// Live rehost check (not part of `npm test`): needs two nodes (scripts/two-nodes.sh) and NET_API_KEY.
// Joins a relay room on node A, waits for `rehost` (A got SIGTERM), rejoins via node B with the
// same grant, then a second client on B must still reach it.  Usage: node test/rehost.mjs
import { Client } from "../dist/index.js";
import WebSocket from "ws";
if (!globalThis.WebSocket) globalThis.WebSocket = WebSocket;
const A = "http://127.0.0.1:8090", B = "http://127.0.0.1:8091", key = process.env.NET_API_KEY;
if (!key) { console.log("rehost: NET_API_KEY not set, skipping live check"); process.exit(process.env.NET_LIVE_REQUIRED ? 1 : 0); }
async function grant(http, player) {
  const r = await fetch(`${http}/v1/grants`, { method: "POST", headers: { authorization: `Bearer ${key}`, "content-type": "application/json" }, body: JSON.stringify({ player_id: player, name: player }) });
  if (!r.ok) throw new Error(`grant failed: ${r.status} ${await r.text()} (is NET_API_KEY from this node's project?)`);
  return (await r.json()).grant;
}
const room = process.argv[2] ?? `web-rehost-${process.pid}`;
const a = await Client.connect(A, await grant(A, "web-rehost-a"));
const j1 = await a.joinOrCreate(room, "relay", 8);
console.log(`web: joined ${room} actor=${j1.actor} on A`);
let frames = 0; a.on("data", () => frames++);
const rehost = new Promise((res) => a.on("rehost", res));
// a client that a random join brought into a room: the room is full with it, and another room it
// could be matched into stands open beside it
const tag = { tag: room };
const host = await Client.connect(A, await grant(A, "web-rehost-h"));
await host.joinOrCreate(`${room}-full`, "relay", 2, tag);
const r = await Client.connect(A, await grant(A, "web-rehost-r"));
const jr1 = await r.joinRandom(tag);
const beside = await Client.connect(A, await grant(A, "web-rehost-o"));
await beside.joinOrCreate(`${room}-open`, "relay", 8, tag);
const moved = new Promise((res) => r.on("rehost", res));
console.log(`web: a random join led to ${jr1.name} actor=${jr1.actor}`);
// a reliable event the room did not get is handed back: none is lost without a word
const listener = await Client.connect(A, await grant(A, "web-rehost-l"));
await listener.join({ t: "join", name: room });
const number = (payload) => new DataView(payload.buffer, payload.byteOffset).getUint32(0);
const reached = new Set(), handedBack = new Set(); let sent = 0;
listener.on("event", (f) => { if (f.code === 2) reached.add(number(f.payload)); });
a.on("undelivered", (events) => { for (const e of events) handedBack.add(number(e.payload)); });
const timer = setInterval(() => {
  a.sendEvent(1, new Uint8Array([1]), "others", false);
  const payload = new Uint8Array(4); new DataView(payload.buffer).setUint32(0, sent++);
  a.sendEvent(2, payload, "others", true); a.flush();
}, 20);
await Promise.race([rehost, new Promise((_, rej) => setTimeout(() => rej(new Error("no rehost within 60 s")), 60000))]);
clearInterval(timer);
console.log(`web: got rehost after ${frames} data frames`);
await new Promise((r) => setTimeout(r, 500));
const neither = [...Array(sent).keys()].filter((n) => !reached.has(n) && !handedBack.has(n));
const both = [...handedBack].filter((n) => reached.has(n));
console.log(`web: reliable events at rehost: sent ${sent}, the room got ${reached.size}, handed back ${handedBack.size}, neither ${neither.length}, both ${both.length}`);
if (a.joined !== undefined || a.actor !== -1) { console.log("WEB_REHOST_FAIL still describes the room the node handed off"); process.exit(1); }
const { client: a2, joined: j2 } = await a.rejoin([B]);
console.log(`web: rejoined actor=${j2.actor} (same actor: ${j2.actor === j1.actor})`);
await moved;
let back = "refused";
try {
  const { client: r2, joined: jr2 } = await r.rejoin([B]);
  back = `${jr2.name} actor=${jr2.actor}`;
  r2.close();
} catch (error) {
  back = `refused: ${error.message}`;
}
const returned = back === `${jr1.name} actor=${jr1.actor}`;
console.log(`web: after a random join the rejoin led to ${back} (same room and actor: ${returned})`);
const got = []; a2.on("event", (f) => got.push(f));
const b = await Client.connect(B, await grant(B, "web-rehost-b"));
await b.join({ t: "join", name: room });
b.sendEvent(7, new TextEncoder().encode("after-rehost"), "others", true); b.flush();
const t0 = Date.now(); while (Date.now() - t0 < 3000 && !got.some((f) => f.code === 7)) await new Promise((r) => setTimeout(r, 20));
const ok = got.some((f) => f.code === 7 && new TextDecoder().decode(f.payload) === "after-rehost");
const pass = ok && j2.actor === j1.actor && returned && neither.length === 0;
console.log(pass ? "WEB_REHOST_OK" : `WEB_REHOST_FAIL events=${got.length}`);
a2.close(); b.close(); host.close(); beside.close(); listener.close(); process.exit(pass ? 0 : 1);
