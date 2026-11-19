import { fileURLToPath } from "node:url";
import WebSocket, { WebSocketServer } from "ws";

// A WebSocket proxy in front of the node at `http` that holds every frame for `delayMs` in each direction.
// `lose(ms)` drops the data frames the clients send in the next `ms` milliseconds; a connection's
// first second is spared, so its data path opens. After `plainHello()` the node's `hello` reaches
// new connections without `supports`, as from a node that lists nothing. After `cut(type)` the next
// control message of that type closes its client's connection instead of going on to the node;
// `cut(type, true)` does it to every one.
export function slowLink(http, delayMs, port = 0) {
  let losingUntil = 0, plain = false, cutOn = null, cutAlways = false;
  const server = new WebSocketServer({ port });
  server.on("connection", (near, request) => {
    const far = new WebSocket(http.replace(/^http/, "ws") + request.url);
    const opened = new Promise((resolve) => far.on("open", resolve));
    const since = Date.now();
    near.on("message", (data, binary) => {
      if (binary && Date.now() < losingUntil && Date.now() - since > 1000) return;
      if (!binary && cutOn && JSON.parse(data.toString()).t === cutOn) { if (!cutAlways) cutOn = null; near.close(); return; }
      setTimeout(async () => { await opened; if (far.readyState === 1) far.send(binary ? data : data.toString()); }, delayMs);
    });
    const text = (data) => {
      const message = JSON.parse(data.toString());
      if (!plain || message.t !== "hello") return data.toString();
      delete message.supports;
      return JSON.stringify(message);
    };
    far.on("message", (data, binary) => setTimeout(() => { if (near.readyState === 1) near.send(binary ? data : text(data)); }, delayMs));
    far.on("close", () => near.close()); near.on("close", () => far.close());
  });
  return { url: `http://127.0.0.1:${server.address().port}`, close: () => server.close(), lose: (ms) => { losingUntil = Date.now() + ms; }, cut: (type, always = false) => { cutOn = type; cutAlways = always; }, plainHello: () => { plain = true; } };
}

// As a process, for the live tests of the other SDKs: node test/slow-link.mjs <delayMs> <port>, then
// NET_SLOW_HTTP=http://127.0.0.1:<port>. The node is NET_HTTP (http://127.0.0.1:8090 by default).
// With <loseMs> <everyMs> after the port it loses the data frames of the first `loseMs` of every
// `everyMs` (NET_LOSSY_HTTP). With `plain` after the port its `hello` has no `supports` (NET_PLAIN_HTTP).
// With `cut <type>` after the port every control message of that type closes its connection (NET_CUT_HTTP).
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const plain = process.argv[4] === "plain";
  const [delayMs, port, loseMs, everyMs] = process.argv.slice(2).map(Number);
  const link = slowLink(process.env.NET_HTTP ?? "http://127.0.0.1:8090", delayMs, port);
  if (plain) link.plainHello();
  else if (process.argv[4] === "cut") link.cut(process.argv[5], true);
  else if (loseMs) setInterval(() => link.lose(loseMs), everyMs);
  console.log(`slow link on http://127.0.0.1:${port}: ${delayMs} ms each way` + (plain ? ", hello without supports" : process.argv[4] === "cut" ? `, a control message ${process.argv[5]} closes its connection` : loseMs ? `, data frames lost for ${loseMs} ms every ${everyMs} ms` : ""));
}
