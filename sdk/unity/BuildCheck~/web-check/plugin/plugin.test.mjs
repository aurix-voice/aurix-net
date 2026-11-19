// The JavaScript plug-in of the package linked by Emscripten, as the editor links it into a Web
// build, against a WebSocket server. `npm test` builds it with the emcc on the PATH: 3.1.8 is
// what Unity 2022.3 ships, 3.1.38 what Unity 6 does.
import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { WebSocketServer } from "ws";
import load from "./build/driver.js";

const lines = [];
let driver, server, url, peer;

async function until(what, hold) {
  for (let i = 0; i < 400 && !hold(); i++) await new Promise((done) => setTimeout(done, 5));
  assert.ok(hold(), `${what}; the plug-in said: ${lines.join(" | ")}`);
}
const said = (line) => until(`no "${line}"`, () => lines.includes(line));
const hex = (bytes) => Buffer.from(bytes).toString("hex");
function send(socket, bytes, text) {
  driver.ccall("driver_send", null, ["number", "array", "number", "number"], [socket, bytes, bytes.length, text ? 1 : 0]);
}
async function open() {
  peer = null;
  const socket = driver.ccall("driver_open", "number", ["string"], [url]);
  await said(`open ${socket}`);
  await until("the server saw no socket", () => peer);
  return socket;
}

before(async () => {
  driver = await load({ print: (line) => lines.push(line) });
  server = new WebSocketServer({ port: 0, host: "127.0.0.1" });
  server.on("connection", (socket) => {
    peer = socket;
    socket.got = [];
    socket.on("message", (data, binary) => socket.got.push([binary, hex(data)]));
  });
  await new Promise((done) => server.on("listening", done));
  url = `ws://127.0.0.1:${server.address().port}/ws`;
});
after(() => server?.close());

test("text goes both ways as text", async () => {
  const socket = await open();
  const json = '{"t":"auth","name":"Аня ✓"}';
  send(socket, Buffer.from(json), true);
  await until("the server got no text", () => peer.got.length === 1);
  assert.deepEqual(peer.got[0], [false, hex(Buffer.from(json))]);
  peer.send(json);
  await said(`message ${socket} text ${hex(Buffer.from(json))}`);
  driver.ccall("driver_close", null, ["number"], [socket]);
});

test("bytes go both ways as they are", async () => {
  const socket = await open();
  const bytes = Uint8Array.from({ length: 300 }, (_, i) => i % 256);
  send(socket, bytes, false);
  await until("the server got no bytes", () => peer.got.length === 1);
  assert.deepEqual(peer.got[0], [true, hex(bytes)]);
  peer.send(bytes);
  peer.send(new Uint8Array(0));
  await said(`message ${socket} binary ${hex(bytes)}`);
  await said(`message ${socket} binary `);
  driver.ccall("driver_close", null, ["number"], [socket]);
});

test("a socket the server closes is closed with its code", async () => {
  const socket = await open();
  peer.close(4001);
  await said(`close ${socket} 4001`);
  send(socket, Buffer.from("late"), true);
});

test("a socket the client closes says nothing more", async () => {
  const socket = await open();
  const server = peer;
  driver.ccall("driver_close", null, ["number"], [socket]);
  await until("the server kept the socket", () => server.readyState === server.CLOSED);
  server.got.length = 0;
  send(socket, Buffer.from("late"), true);
  await new Promise((done) => setTimeout(done, 50));
  assert.deepEqual(server.got, []);
  assert.ok(!lines.some((line) => line.startsWith(`close ${socket} `)));
});

test("timers call back until they are cancelled", async () => {
  const every = driver.ccall("driver_every", "number", ["number"], [10]);
  const once = driver.ccall("driver_after", "number", ["number"], [10]);
  const never = driver.ccall("driver_after", "number", ["number"], [10]);
  driver.ccall("driver_cancel", null, ["number"], [never]);
  const count = (timer) => lines.filter((line) => line === `timer ${timer}`).length;
  await until("the interval did not run", () => count(every) >= 3);
  driver.ccall("driver_cancel", null, ["number"], [every]);
  const ran = count(every);
  await new Promise((done) => setTimeout(done, 60));
  assert.deepEqual([count(every), count(once), count(never)], [ran, 1, 0]);
});
