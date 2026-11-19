import { fileURLToPath } from "node:url";
import WebSocket, { WebSocketServer } from "ws";

// A WebSocket proxy in front of the node at `http` that holds every frame for `delayMs` in each direction.
export function slowLink(http, delayMs, port = 0) {
  const server = new WebSocketServer({ port });
  server.on("connection", (near, request) => {
    const far = new WebSocket(http.replace(/^http/, "ws") + request.url);
    const opened = new Promise((resolve) => far.on("open", resolve));
    near.on("message", (data, binary) => setTimeout(async () => { await opened; if (far.readyState === 1) far.send(binary ? data : data.toString()); }, delayMs));
    far.on("message", (data, binary) => setTimeout(() => { if (near.readyState === 1) near.send(binary ? data : data.toString()); }, delayMs));
    far.on("close", () => near.close()); near.on("close", () => far.close());
  });
  return { url: `http://127.0.0.1:${server.address().port}`, close: () => server.close() };
}

// As a process, for the live tests of the other SDKs: node test/slow-link.mjs <delayMs> <port>, then
// NET_SLOW_HTTP=http://127.0.0.1:<port>. The node is NET_HTTP (http://127.0.0.1:8090 by default).
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [delayMs, port] = process.argv.slice(2).map(Number);
  slowLink(process.env.NET_HTTP ?? "http://127.0.0.1:8090", delayMs, port);
  console.log(`slow link on http://127.0.0.1:${port}: ${delayMs} ms each way`);
}
