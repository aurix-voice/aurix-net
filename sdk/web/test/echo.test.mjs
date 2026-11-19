// Two clients over the WS tunnel against a local node: NET_API_KEY=an_... node --test test/
import { test } from "node:test";
import assert from "node:assert/strict";
import { Channel, Client, Chat, LEAVE_WAIT, MAX_CHAT_TEXT, encodeEvent } from "../dist/index.js";
import WebSocket, { WebSocketServer } from "ws";
import { Link } from "../dist/link.js";
import { slowLink } from "./slow-link.mjs";
import { key } from "./live.mjs";
if (!globalThis.WebSocket) globalThis.WebSocket = WebSocket; // Node < 22

const http = process.env.NET_HTTP ?? "http://127.0.0.1:8090";

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
  assert.deepEqual([a.playerId, a.playerName, b.playerId], ["web-a", "web-a", "web-b"]);
  const room = `web-echo-${process.pid}`;
  const ja = await a.joinOrCreate(room, "relay", 4);
  const jb = await b.joinOrCreate(room, "relay", 4);
  assert.notEqual(ja.actor, jb.actor);
  assert.deepEqual(a.lastJoin, { t: "join", name: room }, "the way back into the room is a join by its name");
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

// A WebSocket proxy in front of the node that stops passing the node's datagrams once `mute()` is called,
// and the client's while `gag(true)` holds.
function mutableLink() {
  let muted = false, gagged = false;
  const server = new WebSocketServer({ port: 0 });
  server.on("connection", (near, request) => {
    const far = new WebSocket(http.replace(/^http/, "ws") + request.url);
    const opened = new Promise((resolve) => far.on("open", resolve));
    near.on("message", async (data, binary) => { await opened; if (far.readyState === 1 && !(binary && gagged)) far.send(binary ? data : data.toString()); });
    far.on("message", (data, binary) => { if (near.readyState === 1 && !(binary && muted)) near.send(binary ? data : data.toString()); });
    far.on("close", () => near.close()); near.on("close", () => far.close());
  });
  return { url: `http://127.0.0.1:${server.address().port}`, mute: () => { muted = true; }, gag: (on) => { gagged = on; }, close: () => server.close() };
}

test("a join the node ends with a kick fails at once with the reason", { skip: !key }, async (t) => {
  // a master that kicks a player as soon as it sees it: this proxy follows the node's `joined`
  // with the `left` of the kick, at once and then 50 ms later, and lets nothing of the data
  // path through
  const server = new WebSocketServer({ port: 0 });
  t.after(() => server.close());
  const asked = [];
  let after = 0;
  server.on("connection", (near, request) => {
    const far = new WebSocket(http.replace(/^http/, "ws") + request.url);
    const opened = new Promise((resolve) => far.on("open", resolve));
    near.on("message", async (data, binary) => { await opened; if (!binary) { asked.push(JSON.parse(data.toString()).t); far.send(data.toString()); } });
    far.on("message", (data, binary) => {
      if (binary) return;
      near.send(data.toString());
      const message = JSON.parse(data.toString());
      const kick = () => near.send(JSON.stringify({ t: "left", reason: "cheater", room: message.room }));
      if (message.t === "joined") after ? setTimeout(kick, after) : kick();
    });
    far.on("close", () => near.close()); near.on("close", () => far.close());
  });
  const lefts = [];
  for (after of [0, 50]) {
    const a = await Client.connect(`http://127.0.0.1:${server.address().port}`, await grant(`web-kicked-at-join-${after}`));
    t.after(() => a.close());
    a.on("control", (message) => { if (message.t === "left") lefts.push(message.reason); });
    const began = performance.now();
    const error = await a.joinOrCreate(`web-kicked-at-join-${process.pid}-${after}`, "relay", 4).catch((error) => error);
    assert.deepEqual([error.code, error.message], ["left", "cheater"]);
    assert.ok(performance.now() - began < 1000, "the join did not wait for a data path that would not open");
    assert.equal(a.joined, undefined);
  }
  await new Promise((resolve) => setTimeout(resolve, 300));
  assert.deepEqual(lefts, [], "the game hears of no room it was never in");
  assert.ok(!asked.includes("leave"), "the client does not leave a room the node took it out of");
});

test("a joined that repeats the id of another join does not answer this one", { skip: !key }, async (t) => {
  // ahead of the node's answer to a join, this proxy sends the answer to a join the client gave up
  const server = new WebSocketServer({ port: 0 });
  t.after(() => server.close());
  server.on("connection", (near, request) => {
    const far = new WebSocket(http.replace(/^http/, "ws") + request.url);
    const opened = new Promise((resolve) => far.on("open", resolve));
    near.on("message", async (data, binary) => { await opened; far.send(binary ? data : data.toString()); });
    far.on("message", (data, binary) => {
      const message = binary ? undefined : JSON.parse(data.toString());
      if (message?.t === "joined") near.send(JSON.stringify({ ...message, name: "given-up", id: message.id + 1 }));
      near.send(binary ? data : data.toString());
    });
    far.on("close", () => near.close()); near.on("close", () => far.close());
  });
  const a = await Client.connect(`http://127.0.0.1:${server.address().port}`, await grant("web-join-id"));
  t.after(() => a.close());
  const room = `web-join-id-${process.pid}`;
  assert.equal((await a.joinOrCreate(room, "relay", 4)).name, room);
  a.leave();
  assert.equal((await a.joinRandom({}, `map = '${room}'`).catch((error) => error)).code, "not_found");
});

test("a lost connection is reported once, also when the handler closes the client", { skip: !key }, async () => {
  const link = mutableLink();
  const a = await Client.connect(link.url, await grant("web-lost"));
  const reasons = [];
  a.on("closed", (why) => { reasons.push(why); a.close(); });
  a.on("undelivered", (events) => reasons.push(events.map((event) => event.code)));
  try {
    await a.joinOrCreate(`web-lost-${process.pid}`, "relay", 4);
    link.mute();
    a.sendEvent(1, new Uint8Array(8), "others", true);
    for (let i = 0; i < 800 && !reasons.length; i++) await new Promise((resolve) => setTimeout(resolve, 25));
    await new Promise((resolve) => setTimeout(resolve, 300));
    assert.deepEqual(reasons, [[1], "link dead"], "the event the node did not acknowledge comes first");
  } finally {
    a.close(); link.close();
  }
});

test("the reliable events the node did not acknowledge are handed back when the room is lost", { skip: !key }, async () => {
  const remove = (path) => fetch(`${http}/v1/rooms/${path}`, { method: "DELETE", headers: { authorization: `Bearer ${key}` } });
  // how: what ends the room for the client; the node's acknowledgements do not reach it
  const run = async (how) => {
    const link = mutableLink();
    const a = await Client.connect(link.url, await grant(`web-undelivered-${how}`));
    const b = await Client.connect(http, await grant(`web-undelivered-${how}-b`));
    const seen = [], got = [];
    a.on("undelivered", (events) => seen.push(events));
    a.on("closed", (why) => seen.push(why));
    a.on("control", (message) => { if (message.t === "left") seen.push(`left: ${message.reason}`); });
    b.on("event", (frame) => got.push(frame.code));
    try {
      const joined = await a.joinOrCreate(`web-undelivered-${process.pid}-${how}`, "relay", 4);
      await b.joinOrCreate(joined.name, "relay", 4);
      a.sendEvent(1, Uint8Array.of(1), "others", true);
      for (let i = 0; i < 80 && !got.length; i++) await new Promise((resolve) => setTimeout(resolve, 25));
      link.mute();
      a.sendEvent(2, Uint8Array.of(2), [b.actor], true);
      a.sendEvent(3, Uint8Array.of(3), "others", false);
      a.sendData(Channel.Reliable, encodeEvent(4, Uint8Array.of(4), "others"));
      a.sendEvent(5, new Uint8Array(3000).fill(5), "master", true, { cache: "add" });
      a.sendEventOn(Channel.ReliableUnordered, 6, Uint8Array.of(6), "all", { group: 9 });
      for (let i = 0; i < 80 && got.length < 4; i++) await new Promise((resolve) => setTimeout(resolve, 25));
      if (how === "kick") await remove(`${joined.room}/players/${joined.actor}`);
      if (how === "socket") a.ws.close();
      if (how === "leave") await a.leave();
      if (how === "leave, socket") { const left = a.leave(); a.ws.close(); await left; }
      await new Promise((resolve) => setTimeout(resolve, 500));
      return { seen, got };
    } finally {
      a.close(); b.close(); link.close();
    }
  };
  const unacknowledged = [
    { code: 2, payload: Uint8Array.of(2), target: [2], group: undefined, cache: undefined },
    { code: 5, payload: new Uint8Array(3000).fill(5), target: "master", group: undefined, cache: "add" },
    { code: 6, payload: Uint8Array.of(6), target: "all", group: 9, cache: undefined },
  ];
  const kick = await run("kick");
  assert.deepEqual(kick.seen, [unacknowledged, "left: kicked"], "kicked: the events come before the left");
  assert.ok(kick.got.includes(2), "the room had got an event it did not acknowledge");
  assert.deepEqual((await run("socket")).seen, [unacknowledged, "control channel closed"], "socket closed: the events come before closed");
  assert.deepEqual((await run("leave, socket")).seen, [unacknowledged, "control channel closed"], "socket closed under a leave that waits: as any closed socket");
  const began = performance.now();
  assert.deepEqual((await run("leave")).seen, [unacknowledged, "left: left"], "a leave the node did not acknowledge in time: the events come before the left");
  assert.ok(performance.now() - began < LEAVE_WAIT + 2000, "the leave waited no longer than LEAVE_WAIT");
});

test("a leave waits for the node to acknowledge what the game sent reliably", { skip: !key }, async () => {
  const link = mutableLink();
  const a = await Client.connect(link.url, await grant("web-leave-waits"));
  const b = await Client.connect(http, await grant("web-leave-waits-b"));
  const seen = [], got = [];
  a.on("undelivered", (events) => seen.push(events));
  a.on("control", (message) => { if (message.t === "left") seen.push(`left: ${message.reason}`); });
  b.on("event", (frame) => got.push(frame.code));
  try {
    const joined = await a.joinOrCreate(`web-leave-waits-${process.pid}`, "relay", 4);
    await b.joinOrCreate(joined.name, "relay", 4);
    // the event is lost on its way to the node; the one the link sends again arrives
    link.gag(true);
    a.sendEvent(7, Uint8Array.of(7), "others", true);
    const left = a.leave();
    assert.throws(() => a.sendEvent(8, Uint8Array.of(8), "others", true), /disconnected/, "a client that is leaving takes no new sends");
    setTimeout(() => link.gag(false), 100);
    await left;
    for (let i = 0; i < 40 && !seen.length; i++) await new Promise((resolve) => setTimeout(resolve, 25));
    assert.deepEqual(got, [7], "the room got the event sent before the leave");
    assert.deepEqual(seen, ["left: left"], "the node acknowledged all of it: nothing is handed back");
    assert.equal((await a.joinOrCreate(`${joined.name}-next`, "relay", 4)).name, `${joined.name}-next`, "the client is free to join again");
  } finally {
    a.close(); b.close(); link.close();
  }
});

test("a data path the node gave up is closed at the client by the node's answer", { skip: !key }, async () => {
  const link = mutableLink();
  const a = await Client.connect(link.url, await grant("web-given-up"));
  const b = await Client.connect(http, await grant("web-given-up-b"));
  const reasons = [], errors = [];
  a.on("closed", (why) => reasons.push(why));
  a.on("control", (message) => { if (message.t === "error") errors.push(`${message.code}: ${message.message}`); });
  try {
    const room = `web-given-up-${process.pid}`;
    await a.joinOrCreate(room, "relay", 4);
    await b.joinOrCreate(room, "relay", 4);
    // the node's reliable messages reach a, a's acknowledgements do not reach the node: a itself
    // sends nothing reliable, so only the node sees the link die
    link.gag(true);
    b.sendEvent(1, new Uint8Array(8), "others", true);
    await new Promise((resolve) => setTimeout(resolve, 17000));
    assert.deepEqual(reasons, []);
    link.gag(false);
    for (let i = 0; i < 5; i++) a.sendEvent(2, new Uint8Array(8), "others", false);
    for (let i = 0; i < 120 && !reasons.length; i++) await new Promise((resolve) => setTimeout(resolve, 25));
    await new Promise((resolve) => setTimeout(resolve, 300));
    assert.deepEqual(errors, ["transport: transport: data path closed: link dead"]);
    assert.deepEqual(reasons, ["link dead"]);
    assert.throws(() => a.sendEvent(2, new Uint8Array(8), "others", true));
  } finally {
    a.close(); b.close(); link.close();
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

test("a connection that closes fails the call waiting on it at once", { skip: !key }, async (t) => {
  const link = slowLink(http, 0);
  t.after(() => link.close());
  const Real = globalThis.WebSocket, sockets = [];
  globalThis.WebSocket = class extends Real { constructor(...args) { super(...args); sockets.push(this); } };
  t.after(() => { globalThis.WebSocket = Real; for (const socket of sockets) socket.close(); });
  const since = Date.now();
  link.cut("auth");
  await assert.rejects(Client.connect(link.url, await grant("web-cut-a")), /control channel closed/);
  const a = await Client.connect(link.url, await grant("web-cut-a"));
  link.cut("join_or_create");
  await assert.rejects(a.joinOrCreate(`web-cut-${process.pid}`, "relay", 4), /control channel closed/);
  assert.ok(Date.now() - since < 5000, "neither call waited for its timeout");
  await new Promise((resolve) => setTimeout(resolve, 300));
  assert.deepEqual(sockets.map((socket) => socket.readyState === Real.OPEN), [false, false]);
});

test("a client outside a room or without its connection refuses what is sent", { skip: !key }, async (t) => {
  const link = slowLink(http, 0);
  t.after(() => link.close());
  const a = await Client.connect(link.url, await grant("web-gone-a"));
  t.after(() => a.close());
  const send = () => a.sendEvent(1, Uint8Array.of(1), "others", true);
  assert.throws(send, /disconnected/);
  await a.joinOrCreate(`web-gone-${process.pid}`, "relay", 4);
  send();
  a.leave();
  assert.throws(send, /disconnected/);
  const joined = await a.joinOrCreate(`web-gone-${process.pid}`, "relay", 4);
  send();
  const left = new Promise((resolve) => a.on("control", (message) => { if (message.t === "left") resolve(); }));
  await fetch(`${http}/v1/rooms/${joined.room}/players/${joined.actor}`, { method: "DELETE", headers: { authorization: `Bearer ${key}` } });
  await left;
  assert.throws(send, /disconnected/);
  await a.joinOrCreate(`web-gone-${process.pid}`, "relay", 4);
  send();
  const closed = new Promise((resolve) => a.on("closed", resolve));
  link.cut("ping");
  a.send({ t: "ping" });
  await closed;
  assert.throws(send, /disconnected/);
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

test("the longest chat text reaches the room whole, a longer one is refused", { skip: !key }, async (t) => {
  const a = await Client.connect(http, await grant("web-text-a"));
  const b = await Client.connect(http, await grant("web-text-b"));
  t.after(() => { a.close(); b.close(); });
  const room = `web-text-${process.pid}`;
  await a.joinOrCreate(room, "relay", 4);
  await b.joinOrCreate(room, "relay", 4);
  const chatA = new Chat(a), chatB = new Chat(b), texts = [];
  chatA.join(1); chatB.join(1);
  chatB.onMessage((message) => texts.push(message.text));
  await new Promise((resolve) => setTimeout(resolve, 200));
  const longest = "😀".repeat(MAX_CHAT_TEXT);
  assert.throws(() => chatA.send(1, longest + "a"), RangeError);
  assert.throws(() => chatA.whisper(2, "a".repeat(MAX_CHAT_TEXT + 1)), RangeError);
  chatA.send(1, longest, false);
  for (let i = 0; i < 100 && texts.length < 1; i++) await new Promise((r) => setTimeout(r, 20));
  assert.deepEqual(texts, [longest]);
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

test("a hidden room is out of the room list and random joins and is entered by its name", { skip: !key }, async (t) => {
  const a = await Client.connect(http, await grant("web-hidden-a"));
  t.after(() => a.close());
  const b = await Client.connect(http, await grant("web-hidden-b"));
  t.after(() => b.close());
  const room = `web-hidden-${process.pid}`;
  await a.joinOrCreate(room, "relay", 4, { map: room }, [], ["map"], false);
  assert.deepEqual(await b.rooms(`map = '${room}'`), []);
  await assert.rejects(b.joinRandom({}, `map = '${room}'`), { code: "not_found" });
  // the room exists: the same call with `visible: true` enters it and leaves it hidden
  assert.equal((await b.joinOrCreate(room, "relay", 4, {}, [], [], true)).name, room);
  const c = await Client.connect(http, await grant("web-hidden-c"));
  t.after(() => c.close());
  assert.deepEqual(await c.rooms(`map = '${room}'`), []);
});

test("a closed room lets its creator in and nobody else", { skip: !key }, async (t) => {
  const a = await Client.connect(http, await grant("web-closed-a"));
  t.after(() => a.close());
  const b = await Client.connect(http, await grant("web-closed-b"));
  t.after(() => b.close());
  const room = `web-closed-${process.pid}`;
  assert.equal((await a.joinOrCreate(room, "relay", 4, {}, [], [], true, false)).name, room);
  await assert.rejects(b.joinOrCreate(room, "relay", 4), { code: "room_closed" });
  await assert.rejects(b.join({ t: "join", name: room }), { code: "room_closed" });
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

test("a message with a field the node does not know is refused, and the field is named", { skip: !key }, async (t) => {
  const a = await Client.connect(http, await grant("web-field-a"));
  t.after(() => a.close());
  const room = `web-field-${process.pid}`;
  await assert.rejects(a.join({ t: "join_or_create", name: room, colour: "red" }), (error) => {
    assert.equal(error.code, "invalid_request");
    assert.match(error.message, /unknown field `colour`/);
    return true;
  });
  // what the game calls its own takes any key
  const joined = await a.join({ t: "join_or_create", name: room, props: { colour: "red" }, player_props: { hat: 1 } });
  assert.equal(joined.props.colour, "red");
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

/** Collects what listeners threw, as the global error handler of a page would get it. */
function reported(t) {
  const errors = [];
  const before = globalThis.reportError;
  globalThis.reportError = (error) => errors.push(error.message);
  t.after(() => { globalThis.reportError = before; });
  return errors;
}

test("a listener that throws keeps the event from no other listener", { skip: !key }, async (t) => {
  const errors = reported(t);
  const a = await Client.connect(http, await grant("web-throw-a"));
  const b = await Client.connect(http, await grant("web-throw-b"));
  t.after(() => { a.close(); b.close(); });
  const room = `web-throw-${process.pid}`;
  await a.joinOrCreate(room, "relay", 4); await b.joinOrCreate(room, "relay", 4);
  const got = [];
  a.on("event", (frame) => { if (frame.code === 1 && frame.payload[0] === 0) throw new TypeError("a bug in the game"); });
  a.on("event", (frame) => { if (frame.code === 1) got.push(frame.payload[0]); });
  const chatA = new Chat(a), chatB = new Chat(b);
  chatA.join(1); chatB.join(1);
  chatA.onMessage(() => { throw new TypeError("a bug in the chat"); });
  chatA.onMessage((message) => got.push(message.text));
  for (const n of [0, 1]) b.sendEvent(1, Uint8Array.of(n), "others", true);
  chatB.send(1, "hello");
  for (let i = 0; i < 100 && got.length < 3; i++) await new Promise((r) => setTimeout(r, 20));
  assert.deepEqual([got, errors], [[0, 1, "hello"], ["a bug in the game", "a bug in the chat"]]);
});

test("a listener that throws does not stop the data path over WebTransport", { skip: !key }, async (t) => {
  const errors = reported(t);
  // stands in for the browser's WebTransport; the node's end of the link is held here
  const far = new Link();
  let feed;
  const send = () => { for (const bytes of far.drain()) feed.enqueue(bytes); };
  globalThis.WebTransport = class {
    ready = Promise.resolve();
    closed = new Promise(() => {});
    datagrams = {
      readable: new ReadableStream({ start: (controller) => { feed = controller; } }),
      writable: new WritableStream({ write: (bytes) => { far.receive(bytes); send(); } }),
    };
    close() {}
  };
  t.after(() => { delete globalThis.WebTransport; });
  const a = await Client.connect(http, await grant("web-throw-wt"));
  t.after(() => a.close());
  const joining = a.joinOrCreate(`web-throw-wt-${process.pid}`, "relay", 4);
  // the node answers the ticket with an empty message
  const answer = setInterval(() => { if (feed) { clearInterval(answer); far.send(Channel.Reliable, new Uint8Array(0)); send(); } }, 50);
  await joining;
  const got = [];
  a.on("data", (channel, tick, payload) => { if (payload[0] === 0) throw new TypeError("a bug in the game"); got.push(payload[0]); });
  for (const n of [0, 1, 2]) { far.send(Channel.Reliable, Uint8Array.of(n)); send(); await new Promise((r) => setTimeout(r, 20)); }
  for (let i = 0; i < 100 && got.length < 2; i++) await new Promise((r) => setTimeout(r, 20));
  assert.deepEqual([got, errors], [[1, 2], ["a bug in the game"]]);
});
