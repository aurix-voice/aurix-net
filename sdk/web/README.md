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

## Sync rooms

`SyncSession` turns a joined sync-room client into a replicated world (same wire format as the Unity
managed core, checked against shared golden vectors): schema IDL parsing, delta decoding with acks,
owner-authoritative writes or raw inputs for server logic, and an interpolation buffer for remotes.

```ts
import { Client, SyncSession, ComponentValue, InterpolationBuffer, lerpVec, distVec } from "@aurix/net";
const schema = "component Transform {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n}\n";
const c = await Client.connect(url, grant);
const joined = await c.joinOrCreate("arena-1", "sync", 16, { schema });
const s = new SyncSession(c, schema, 60 /* tick Hz */, 2 /* interpolation ticks */);
const tr = s.world.schema.byName("Transform")!;
const remote = new Map<number, InterpolationBuffer<number[]>>();
s.world.onChange = (e) => { if (e.owner !== joined.actor) (remote.get(e.id) ?? remote.set(e.id, new InterpolationBuffer(lerpVec, distVec, 5)).get(e.id)!).push(e.lastTick, s.world.get(e, tr)!.fields[0] as number[]); };
setInterval(() => s.sendWrites([[tr, new ComponentValue([[x, y, z], [0, 0, 0, 1]])]]), 1000 / 60); // my avatar
function frame(dt: number) { const t = s.advance(dt); for (const [, b] of remote) draw(b.sample(t)); }
```

With a WASM `logic` module driving entities, send raw inputs (`s.sendInput(bytes)` returns the tick) and
reconcile your own entity when its server state arrives (`onChange` where `e.owner === joined.actor`).

## Data path

In browsers the client opens a WebTransport session to `joined.data.webtransport_url`
(HTTP/3 datagrams, certificate pinned via `serverCertificateHashes`, so the dev server's
self-signed certificate works without installing anything) and falls back to the WebSocket
binary tunnel when WebTransport is unavailable or fails. Node (tests) uses the tunnel.
`client.dataPath` tells you which one is active; set `client.forceTunnel = true` to skip
WebTransport.

`Client.connectAndJoin(httpUrl, grant, msg)` connects and joins in one call and follows a
`room_elsewhere:<url>` redirect to the node hosting the room (rooms are pinned to one node).
