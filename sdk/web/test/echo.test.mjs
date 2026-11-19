// Two clients over the WS tunnel against a local node: NET_API_KEY=an_... node --test test/
import { test } from "node:test";
import assert from "node:assert/strict";
import { Channel, Client, Chat, encodeEvent } from "../dist/index.js";
import WebSocket, { WebSocketServer } from "ws";
import { slowLink } from "./slow-link.mjs";
if (!globalThis.WebSocket) globalThis.WebSocket = WebSocket; // Node < 22

const http = process.env.NET_HTTP ?? "http://127.0.0.1:8090";
const key = process.env.NET_API_KEY;

async function grant(player) {
  const r = await fetch(`${http}/v1/grants`, { method: "POST", headers: { authorization: `Bearer ${key}`, "content-type": "application/json" }, body: JSON.stringify({ player_id: player, name: player }) });
  if (!r.ok) throw new Error(`grant failed: ${r.status} ${await r.text()} (is NET_API_KEY from this node's project?)`);
  return (await r.json()).grant;
}

test("relay echo over ws tunnel", { skip: !key }, async (t) => {
  const a = await Client.connect(http, await grant("web-a"));
  t.after(() => a.close());
  const b = await Client.connect(http, await grant("web-b"));
  t.after(() => b.close());
  const room = `web-echo-${process.pid}`;
  const ja = await a.joinOrCreate(room, "relay", 4);
  const jb = await b.joinOrCreate(room, "relay", 4);
  assert.notEqual(ja.actor, jb.actor);
  const got = []; b.on("event", (f) => got.push(f));
  const bData = []; b.on("data", (channel, tick, payload) => bData.push({ channel, payload }));
  a.sendEvent(1, new TextEncoder().encode("hello"), "others", true);
  for (let i = 0; i < 10; i++) { a.sendEvent(2, new Uint8Array([i]), "others", false); a.flush(); await new Promise((r) => setTimeout(r, 16)); }
  const t0 = Date.now();
  while (Date.now() - t0 < 3000 && got.filter((f) => f.code === 2).length < 10) await new Promise((r) => setTimeout(r, 20));
  const hello = got.find((f) => f.code === 1);
  assert.ok(hello && new TextDecoder().decode(hello.payload) === "hello" && hello.from === ja.actor);
  assert.equal(got.filter((f) => f.code === 2).length, 10);
  // fragmentation (5 KB reliable) + reliable-unordered
  const big = new Uint8Array(5000).map((_, i) => i % 251);
  a.sendEvent(3, big, "others", true);
  a.sendData(2, encodeEvent(4, new Uint8Array([9, 9, 9]), "others")); // reliable-unordered channel
  await new Promise((r) => setTimeout(r, 1000));
  const bigF = got.find((f) => f.code === 3);
  assert.ok(bigF && bigF.payload.length === 5000 && bigF.payload.every((v, i) => v === i % 251), "fragmented event");
  assert.ok(bData.some((d) => d.channel === 2) && got.some((f) => f.code === 4 && f.payload[0] === 9), "reliable-unordered event");
  a.leave();
});

test("a client the node took out of its room no longer describes it", { skip: !key }, async (t) => {
  const a = await Client.connect(http, await grant("web-out"));
  t.after(() => a.close());
  const lefts = [];
  a.on("control", (message) => { if (message.t === "left") lefts.push(message.reason); });
  const remove = (path) => fetch(`${http}/v1/rooms/${path}`, { method: "DELETE", headers: { authorization: `Bearer ${key}` } });
  for (const how of ["kicked", "room closed", "idle too long"]) {
    const joined = await a.joinOrCreate(`web-out-${process.pid}-${how.length}`, "lockstep", 4);
    a.onMessage(JSON.stringify({ t: "left", reason: how, room: "a-room-left-earlier" }));
    assert.equal(lefts.pop(), how);
    assert.ok(a.joined === joined && a.actor === joined.actor, `${how}: a late message about another room took the room away`);
    const kick = `${joined.room}/players/${joined.actor}`;
    const answer = await remove({ kicked: kick, "room closed": joined.room }[how] ?? `${kick}?reason=${encodeURIComponent(how)}`);
    assert.equal(answer.status, 204);
    await new Promise((r) => setTimeout(r, 300));
    assert.equal(lefts.pop(), how);
    assert.ok(a.joined === undefined && a.actor === -1, `${how}: the client still describes the room`);
  }
});

test("leave and join another room on the same client", { skip: !key }, async (t) => {
  const a = await Client.connect(http, await grant("web-a"));
  t.after(() => a.close());
  const b = await Client.connect(http, await grant("web-b"));
  t.after(() => b.close());
  await a.joinOrCreate(`web-first-${process.pid}`, "relay", 4);
  for (let i = 0; i < 5; i++) a.sendEvent(1, new Uint8Array([i]), "all", true);
  await new Promise((r) => setTimeout(r, 300));
  a.leave();
  assert.ok(a.joined === undefined && a.actor === -1, "the client still describes the room it left");
  await new Promise((r) => setTimeout(r, 300));
  const room = `web-second-${process.pid}`;
  await a.joinOrCreate(room, "relay", 4);
  await b.joinOrCreate(room, "relay", 4);
  const got = []; b.on("event", (f) => got.push(f));
  for (let i = 0; i < 5; i++) a.sendEvent(2, new Uint8Array([i]), "others", true);
  const t0 = Date.now();
  while (Date.now() - t0 < 3000 && got.length < 5) await new Promise((r) => setTimeout(r, 20));
  assert.deepEqual(got.map((f) => f.payload[0]), [0, 1, 2, 3, 4]);
});

// A WebSocket proxy in front of the node that stops passing the node's datagrams once `mute()` is called.
function mutableLink() {
  let muted = false;
  const server = new WebSocketServer({ port: 0 });
  server.on("connection", (near, request) => {
    const far = new WebSocket(http.replace(/^http/, "ws") + request.url);
    const opened = new Promise((resolve) => far.on("open", resolve));
    near.on("message", async (data, binary) => { await opened; if (far.readyState === 1) far.send(binary ? data : data.toString()); });
    far.on("message", (data, binary) => { if (near.readyState === 1 && !(binary && muted)) near.send(binary ? data : data.toString()); });
    far.on("close", () => near.close()); near.on("close", () => far.close());
  });
  return { url: `http://127.0.0.1:${server.address().port}`, mute: () => { muted = true; }, close: () => server.close() };
}

test("a lost connection is reported once, also when the handler closes the client", { skip: !key }, async () => {
  const link = mutableLink();
  const a = await Client.connect(link.url, await grant("web-lost"));
  const reasons = [];
  a.on("closed", (why) => { reasons.push(why); a.close(); });
  try {
    await a.joinOrCreate(`web-lost-${process.pid}`, "relay", 4);
    link.mute();
    a.sendEvent(1, new Uint8Array(8), "others", true);
    for (let i = 0; i < 400 && !reasons.length; i++) await new Promise((resolve) => setTimeout(resolve, 25));
    await new Promise((resolve) => setTimeout(resolve, 300));
    assert.deepEqual(reasons, ["link dead"]);
  } finally {
    a.close(); link.close();
  }
});

test("a failed connectAndJoin closes its connection", { skip: !key }, async () => {
  const Real = globalThis.WebSocket, sockets = [];
  globalThis.WebSocket = class extends Real { constructor(...args) { super(...args); sockets.push(this); } };
  try {
    await assert.rejects(Client.connectAndJoin(http, await grant("web-leak"), { t: "join", name: `web-no-such-room-${process.pid}` }), /not found/);
    await new Promise((resolve) => setTimeout(resolve, 300));
    assert.deepEqual(sockets.map((socket) => socket.readyState === Real.OPEN), [false]);
  } finally {
    globalThis.WebSocket = Real;
    for (const socket of sockets) socket.close();
  }
});

test("a late joiner gets the cached events and the chat history", { skip: !key }, async () => {
  const a = await Client.connect(http, await grant("web-cache-a"));
  const b = await Client.connect(http, await grant("web-cache-b"));
  try {
    const room = `web-cache-${process.pid}`;
    const ja = await a.joinOrCreate(room, "relay", 4);
    const chatA = new Chat(a); chatA.join(1);
    a.sendEvent(20, Uint8Array.of(7), "others", true, { cache: "add" });
    chatA.send(1, "hello");
    await new Promise((resolve) => setTimeout(resolve, 300));
    const events = [], messages = [];
    b.on("event", (frame) => events.push([frame.code, frame.from, frame.cached]));
    await b.joinOrCreate(room, "relay", 4);
    const chatB = new Chat(b); chatB.onMessage((message) => messages.push(message)); chatB.join(1);
    assert.deepEqual(events, [[20, ja.actor, true], [chatA.code, ja.actor, true]]);
    assert.deepEqual(messages, [{ from: ja.actor, channel: 1, text: "hello", cached: true }]);
  } finally {
    a.close(); b.close();
  }
});

test("rooms(expr) finds a room by the lobby props joinOrCreate published", { skip: !key }, async () => {
  const a = await Client.connect(http, await grant("web-lobby-a"));
  const b = await Client.connect(http, await grant("web-lobby-b"));
  try {
    const room = `web-lobby-${process.pid}`;
    await a.joinOrCreate(room, "relay", 4, { map: "dust", secret: 1 }, [], ["map"]);
    const found = (await b.rooms("map = 'dust'")).find((entry) => entry.name === room);
    assert.deepEqual(found?.props, { map: "dust" });
  } finally {
    a.close(); b.close();
  }
});

test("events on the unsequenced channel arrive on it", { skip: !key }, async (t) => {
  const a = await Client.connect(http, await grant("web-unseq-a"));
  t.after(() => a.close());
  const b = await Client.connect(http, await grant("web-unseq-b"));
  t.after(() => b.close());
  const room = `web-unseq-${process.pid}`;
  await a.joinOrCreate(room, "relay", 4);
  await b.joinOrCreate(room, "relay", 4);
  const channels = []; b.on("data", (channel) => channels.push(channel));
  const codes = []; b.on("event", (frame) => codes.push(frame.code));
  await new Promise((resolve) => setTimeout(resolve, 300));
  for (let i = 0; i < 10; i++) { a.sendEventOn(Channel.UnreliableUnsequenced, 9, new Uint8Array([i])); a.flush(); await new Promise((resolve) => setTimeout(resolve, 16)); }
  await new Promise((resolve) => setTimeout(resolve, 300));
  assert.deepEqual(codes, Array(10).fill(9));
  assert.deepEqual(channels, Array(10).fill(Channel.UnreliableUnsequenced));
});

test("a node that does not list the unsequenced channel gets its events on the sequenced one", { skip: !key }, async (t) => {
  const link = slowLink(http, 0); link.plainHello();
  t.after(() => link.close());
  const a = await Client.connect(link.url, await grant("web-plain-a"));
  t.after(() => a.close());
  const b = await Client.connect(http, await grant("web-plain-b"));
  t.after(() => b.close());
  const room = `web-plain-${process.pid}`;
  await a.joinOrCreate(room, "relay", 4);
  await b.joinOrCreate(room, "relay", 4);
  const channels = []; b.on("data", (channel) => channels.push(channel));
  await new Promise((resolve) => setTimeout(resolve, 300));
  for (let i = 0; i < 10; i++) { a.sendEventOn(Channel.UnreliableUnsequenced, 9, new Uint8Array([i])); a.flush(); await new Promise((resolve) => setTimeout(resolve, 16)); }
  await new Promise((resolve) => setTimeout(resolve, 300));
  assert.deepEqual(channels, Array(10).fill(Channel.Unreliable));
});

test("two lookups in flight: each call gets its own answer or refusal", { skip: !key }, async () => {
  const a = await Client.connect(http, await grant("web-ids-a"));
  const b = await Client.connect(http, await grant("web-ids-b"));
  const c = await Client.connect(http, await grant("web-ids-c"));
  try {
    const dust = `web-ids-dust-${process.pid}`, nuke = `web-ids-nuke-${process.pid}`;
    await a.joinOrCreate(dust, "relay", 4, { map: "dust" }, [], ["map"]);
    await b.joinOrCreate(nuke, "relay", 4, { map: "nuke" }, [], ["map"]);
    const mine = (rooms) => rooms.map((entry) => entry.name).filter((name) => name === dust || name === nuke);
    // the node answers one lookup a second: the second of two sent at once is refused
    const [first, second] = await Promise.allSettled([c.rooms("map = 'dust'").then(mine), c.rooms("map = 'nuke'").then(mine)]);
    assert.deepEqual(first.value, [dust]);
    assert.equal(second.reason?.code, "conflict", `the second call got ${JSON.stringify(second.value)}`);
    await new Promise((resolve) => setTimeout(resolve, 1100));
    // a join in flight is not refused by the refusal of a lookup
    const [, refused, joined] = await Promise.allSettled([c.lobbyStats(), c.lobbyStats(), c.joinOrCreate(dust, "relay", 4)]);
    assert.equal(refused.reason?.code, "conflict");
    assert.equal(joined.value?.name ?? joined.reason?.message, dust);
  } finally {
    a.close(); b.close(); c.close();
  }
});

test("a client that left a room over a 200 ms round trip joins another one", { skip: !key }, async () => {
  const link = slowLink(http, 100);
  const a = await Client.connect(link.url, await grant("web-leave-a"));
  const b = await Client.connect(link.url, await grant("web-leave-b"));
  let chatter;
  try {
    const room = `web-leave-${process.pid}`;
    await a.joinOrCreate(room, "relay", 4); await b.joinOrCreate(room, "relay", 4);
    const errors = [];
    a.on("control", (message) => { if (message.t === "error") errors.push(message.message); });
    // events on their way to `a` when it leaves must not make it answer the room it has left
    chatter = setInterval(() => b.sendEvent(3, Uint8Array.of(9), "others", true), 10);
    await new Promise((r) => setTimeout(r, 300));
    a.leave();
    await new Promise((r) => setTimeout(r, 300));
    await a.joinOrCreate(`${room}-2`, "relay", 4);
    assert.deepEqual(errors, []);
  } finally {
    clearInterval(chatter); a.close(); b.close(); link.close();
  }
});
