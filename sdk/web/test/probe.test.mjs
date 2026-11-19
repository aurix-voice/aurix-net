// The client's own round-trip probes, against a node made up here: node --test test/
import { test } from "node:test";
import assert from "node:assert/strict";
import { Client } from "../dist/index.js";
import WebSocket, { WebSocketServer } from "ws";
if (!globalThis.WebSocket) globalThis.WebSocket = WebSocket; // Node < 22

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

// A node that answers a ping only when the next one arrives, as a link slower than the probes would.
function lateNode() {
  const server = new WebSocketServer({ port: 0 });
  const pings = [];
  server.on("connection", (socket) => {
    let held = null;
    socket.on("message", (data) => {
      const message = JSON.parse(data.toString());
      if (message.t === "auth") socket.send(JSON.stringify({ t: "authed", player_id: "ann", name: "Ann" }));
      if (message.t !== "ping") return;
      pings.push(message.ts);
      if (held !== null) socket.send(JSON.stringify({ t: "pong", ts: held, server_ms: 0 }));
      held = message.ts;
    });
  });
  return { url: `http://127.0.0.1:${server.address().port}`, pings, close: () => server.close() };
}

test("a client probes the control channel by itself and keeps the answers", async () => {
  const node = lateNode();
  const client = await Client.connect(node.url, "grant");
  try {
    const pongs = [];
    client.on("control", (message) => { if (message.t === "pong") pongs.push(message.ts); });
    await sleep(2300);
    client.send({ t: "ping", ts: 987654321 });
    await sleep(1200);
    assert.ok(node.pings.length >= 4, `pings sent: ${node.pings.length}`);
    assert.equal(new Set(node.pings).size, node.pings.length, "every probe carries a time of its own");
    assert.ok(client.rttMs > 0, "the late answers are measured");
    assert.deepEqual(pongs, [987654321], "only the game's own ping is answered to the game");
  } finally {
    client.close();
    node.close();
  }
});
