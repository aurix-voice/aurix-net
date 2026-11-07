# @aurix/net

```ts
import { Client } from "@aurix/net";
const client = await Client.connect("https://<node>", grantFromYourBackend);
client.on("event", (f) => console.log(f.code, f.from, f.payload));
await client.joinOrCreate("lobby-1", "relay", 8);
client.sendEvent(1, new TextEncoder().encode("hi"));   // unreliable
client.flush();                                        // once per frame
```

Control over WebSocket, data over the WebSocket tunnel with the same channels/acks as the native
transport (WebTransport datagrams are the next step). `sendData(Channel.Unreliable, bytes)` for
sync inputs / lockstep frames; raw frames arrive on `"data"`. Build: `npm i && npm run build`,
test against a local node: `NET_API_KEY=an_... npm test`.

## Data path

In browsers the client opens a WebTransport session to `joined.data.webtransport_url`
(HTTP/3 datagrams, certificate pinned via `serverCertificateHashes`, so the dev server's
self-signed certificate works without installing anything) and falls back to the WebSocket
binary tunnel when WebTransport is unavailable or fails. Node (tests) uses the tunnel.
`client.dataPath` tells you which one is active; set `client.forceTunnel = true` to skip
WebTransport.
