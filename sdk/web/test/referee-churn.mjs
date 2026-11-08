// Memory/desync churn probe for a referee (server-simulated) lockstep room; not a test.
// NET_HTTP=http://127.0.0.1:8099 NET_API_KEY=<key of the project owning module physics-arena> \
// NET_LOGIC_PROPS='{"schema":"…","logic":"physics-arena","position":"Transform.0"}' \
// LIE=1 PLAYERS=2 SECS=5 node test/referee-churn.mjs   (LIE=1: the second client lies about its hash every 10 ticks)
import { Client, LockstepClient } from "../dist/index.js";
import WebSocket from "ws";
if (!globalThis.WebSocket) globalThis.WebSocket = WebSocket;
const http = process.env.NET_HTTP, key = process.env.NET_API_KEY;
const hdr = { authorization: `Bearer ${key}`, "content-type": "application/json" };
const LIE = process.env.LIE === "1", REST = process.env.REST === "1", N = +(process.env.PLAYERS ?? 2), SECS = +(process.env.SECS ?? 5);
async function grant(p) { const r = await fetch(`${http}/v1/grants`, { method: "POST", headers: hdr, body: JSON.stringify({ player_id: p, name: p, scopes: ["join", "create"] }) }); if (!r.ok) throw new Error(`grant failed: ${r.status} ${await r.text()} (is NET_API_KEY from this node's project?)`);
  const j = await r.json(); return j.grant ?? j.token; }
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const room = `probe-${process.pid}`; const peers = [];
for (let i = 0; i < N; i++) {
  const name = `pr-${i}`; const c = await Client.connect(http, await grant(name));
  const joined = await c.joinOrCreate(room, "lockstep", 8, { tick_hz: 30, logic: "physics-arena", ...JSON.parse(process.env.NET_LOGIC_PROPS) });
  const ls = new LockstepClient((ch, p) => c.sendData(ch, p));
  ls.onTick = (t) => { if (LIE && i === 1 && t % 10 === 0 && t > 60) ls.reportHash(t, 0xbadn); };
  ls.onDumpRequested = (t) => ls.sendDump(t, 0xbadn, new TextEncoder().encode(`${name}@${t}`));
  c.on("data", (_ch, _tick, payload) => ls.receive(payload));
  peers.push({ c, ls, joined });
}
const timer = setInterval(() => { for (const p of peers) p.ls.sendInput(Uint8Array.of(127, 0, 0)); }, 1000 / 30);
await sleep(SECS * 1000); clearInterval(timer);
if (REST) { const id = peers[0].joined.room; await fetch(`${http}/v1/rooms/${id}`, { headers: hdr }); const l = await (await fetch(`${http}/v1/rooms/${id}/desyncs`, { headers: hdr })).json(); console.log("desyncs", (l.desyncs ?? []).length); }
for (const p of peers) p.c.close();
process.exit(0);
