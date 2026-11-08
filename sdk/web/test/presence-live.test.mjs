import { test } from "node:test";
import assert from "node:assert/strict";
import { Client } from "../dist/index.js";
import WebSocket from "ws";
if (!globalThis.WebSocket) globalThis.WebSocket = WebSocket;

// Two nodes of one fleet: a friend in a room on B, a friend only connected on B, a stranger nowhere;
// the asking client sits on A. NET_HTTP_B defaults to NET_HTTP (single node still passes).
const httpA = process.env.NET_HTTP ?? "http://127.0.0.1:8098";
const httpB = process.env.NET_HTTP_B ?? httpA;
const key = process.env.NET_API_KEY;
const hdr = { authorization: `Bearer ${key}`, "content-type": "application/json" };
async function grant(http, player) {
  const r = await fetch(`${http}/v1/grants`, { method: "POST", headers: hdr, body: JSON.stringify({ player_id: player, name: player, scopes: ["join", "create"] }) });
  const j = await r.json(); return j.grant ?? j.token;
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

test("presence: find_friends across nodes and lobby stats", { skip: !key }, async () => {
  const tag = process.pid;
  const inRoom = await Client.connect(httpB, await grant(httpB, `pf-room-${tag}`));
  const joined = await inRoom.joinOrCreate(`presence-${tag}`, "relay", 4, { lobby: "ranked" }, []);
  const idle = await Client.connect(httpB, await grant(httpB, `pf-idle-${tag}`));
  const asker = await Client.connect(httpA, await grant(httpA, `pf-ask-${tag}`));
  await sleep(300);

  const friends = await asker.findFriends([`pf-room-${tag}`, `pf-idle-${tag}`, `pf-ghost-${tag}`, `pf-room-${tag}`]);
  assert.equal(friends.length, 3, JSON.stringify(friends));
  const [a, b, c] = friends;
  assert.equal(a.player, `pf-room-${tag}`);
  assert.ok(a.online && a.room, JSON.stringify(a));
  assert.equal(a.room.id, joined.room);
  assert.equal(a.room.name, `presence-${tag}`);
  assert.equal(a.room.mode, "relay");
  assert.ok(b.online && !b.room, JSON.stringify(b));
  assert.ok(!c.online && !c.room, JSON.stringify(c));

  await assert.rejects(asker.findFriends([`pf-idle-${tag}`]), /lookup_rate/);
  await sleep(1100);
  const stats = await asker.lobbyStats();
  assert.ok(stats.rooms >= 1 && stats.players >= 1, JSON.stringify(stats));
  const ranked = stats.lobbies.find((l) => l.lobby === "ranked");
  assert.ok(ranked && ranked.rooms >= 1 && ranked.players >= 1, JSON.stringify(stats));

  idle.close();
  await sleep(300);
  await sleep(800);
  const again = await asker.findFriends([`pf-idle-${tag}`]);
  assert.ok(!again[0].online, JSON.stringify(again));
  inRoom.close(); asker.close();
});
