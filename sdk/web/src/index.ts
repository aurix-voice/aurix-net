import { Channel } from "./packet.js";
import { Link, Received } from "./link.js";
import { encodeEvent, decodeEvent, Target } from "./event.js";
export { Channel } from "./packet.js";
export { Link } from "./link.js";
export { encodeEvent, decodeEvent } from "./event.js";
export type { Target, EventFrame } from "./event.js";

export type RoomMode = "relay" | "sync" | "lockstep";
export interface Joined { room: string; name: string; mode: RoomMode; actor: number; master: number; tick_hz?: number; schema_hash?: string; data: { host: string; port: number; cert_sha256: string; ticket: string; ws_tunnel: boolean; webtransport_url?: string } }

/** Data path: WebTransport datagrams when the browser and the node support it, else the WS tunnel. */
interface DataPath { send(d: Uint8Array): void; close(): void; kind: "webtransport" | "ws-tunnel" }


type Listener = (...a: any[]) => void;

/** Browser/Node client. Control over WebSocket; data over WebTransport datagrams (browsers)
 *  or the WebSocket tunnel (fallback / Node). Set `forceTunnel` to skip WebTransport. */
export class Client {
  private ws!: WebSocket; private link = new Link(); private timer?: ReturnType<typeof setInterval>;
  private path?: DataPath; forceTunnel = false;
  /** Which data path is in use after join(). */
  get dataPath() { return this.path?.kind; }
  private listeners = new Map<string, Listener[]>(); private waiters: ((m: any) => boolean)[] = [];
  joined?: Joined; actor = -1;

  on(ev: "control" | "data" | "event" | "ready" | "closed" | "rehost", fn: Listener) { (this.listeners.get(ev) ?? this.listeners.set(ev, []).get(ev)!).push(fn); return this; }
  private emit(ev: string, ...a: any[]) { for (const f of this.listeners.get(ev) ?? []) f(...a); }

  /** URL given to connect(), the grant and the last join message: what rejoin() needs. */
  entryUrl = ""; grant = ""; lastJoin: object | null = null; private opts: { timeoutMs?: number } = {};
  static async connect(httpUrl: string, grant: string, opts: { timeoutMs?: number } = {}): Promise<Client> {
    const c = new Client(); c.entryUrl = httpUrl; c.grant = grant; c.opts = opts;
    const url = httpUrl.replace(/^http/, "ws") + "/ws";
    await new Promise<void>((res, rej) => {
      const t = setTimeout(() => rej(new Error("timeout")), opts.timeoutMs ?? 10000);
      c.ws = new WebSocket(url); c.ws.binaryType = "arraybuffer";
      c.ws.onopen = () => { clearTimeout(t); res(); };
      c.ws.onerror = (e) => { clearTimeout(t); rej(new Error("websocket error")); };
    });
    c.ws.onmessage = (m) => c.onMessage(m.data);
    c.ws.onclose = () => { c.stop(); c.emit("closed", "control channel closed"); };
    c.send({ t: "auth", grant });
    await c.waitFor((m) => m.t === "authed", (m) => m.t === "error");
    return c;
  }

  private onMessage(data: any) {
    if (typeof data === "string") {
      const m = JSON.parse(data);
      this.emit("control", m);
      if (m.t === "left" && m.reason === "rehost") this.emit("rehost");
      this.waiters = this.waiters.filter((w) => !w(m));
    } else {
      this.onDatagram(new Uint8Array(data as ArrayBuffer));
    }
  }
  private waitFor(ok: (m: any) => boolean, fail?: (m: any) => boolean, timeoutMs = 10000): Promise<any> {
    return new Promise((res, rej) => {
      const t = setTimeout(() => rej(new Error("timeout")), timeoutMs);
      this.waiters.push((m) => {
        if (ok(m)) { clearTimeout(t); res(m); return true; }
        if (fail?.(m)) { clearTimeout(t); rej(new Error(`${m.code}: ${m.message}`)); return true; }
        return false;
      });
    });
  }

  /** Any control message, e.g. {t:"set_room_props", props:{...}}. */
  send(msg: object) { this.ws.send(JSON.stringify(msg)); }

  /** connect + join, following one `room_elsewhere:<url>` redirect to the node hosting the room. */
  static async connectAndJoin(httpUrl: string, grant: string, msg: object, opts: { timeoutMs?: number } = {}): Promise<{ client: Client; joined: Joined }> {
    const c = await Client.connect(httpUrl, grant, opts);
    try { return { client: c, joined: await c.join(msg) }; }
    catch (e) {
      const m = String((e as Error).message ?? e);
      const i = m.indexOf("room_elsewhere:");
      if (i < 0) throw e;
      c.close();
      const url = m.slice(i + "room_elsewhere:".length).trim();
      const c2 = await Client.connect(url, grant, opts);
      return { client: c2, joined: await c2.join(msg) };
    }
  }
  async joinOrCreate(name: string, mode: RoomMode = "relay", maxPlayers = 8, props: Record<string, unknown> = {}): Promise<Joined> {
    return this.join({ t: "join_or_create", name, mode, max_players: maxPlayers, props });
  }
  /** After `left{reason:"rehost"}` (the node shut down and handed the room to another node): a fresh
   *  client in the same room with the same grant. Tries the entry URL, then `fallbackUrls`. */
  async rejoin(fallbackUrls: string[] = []): Promise<{ client: Client; joined: Joined }> {
    if (!this.lastJoin) throw new Error("rejoin: never joined");
    const msg = { ...(this.lastJoin as Record<string, unknown>) };
    if (msg.t === "join_or_create") msg.t = "join";
    this.close();
    let err: unknown = null;
    for (const url of [this.entryUrl, ...fallbackUrls]) {
      try { return await Client.connectAndJoin(url, this.grant, msg, this.opts); } catch (e) { err = e; }
    }
    throw err;
  }
  async join(msg: object): Promise<Joined> {
    this.lastJoin = msg;
    this.send(msg);
    const j = (await this.waitFor((m) => m.t === "joined", (m) => m.t === "error")) as Joined;
    this.joined = j; this.actor = j.actor;
    // open the data path: ticket as the first datagram/frame, wait for the server's empty ack
    const ticket = Uint8Array.from(atob(j.data.ticket), (c) => c.charCodeAt(0));
    const ready = new Promise<void>((res, rej) => { const t = setTimeout(() => rej(new Error("data path timeout")), 10000); this.on("ready", () => { clearTimeout(t); res(); }); });
    this.path = await this.openPath(j.data);
    this.path.send(ticket);
    this.timer = setInterval(() => { if (!this.link.poll()) { this.stop(); this.emit("closed", "link dead"); } this.flushLink(); }, 5);
    await ready;
    return j;
  }

  private onDatagram(bytes: Uint8Array) {
    const recv: Received[] = this.link.receive(bytes);
    this.flushLink();
    for (const r of recv) {
      if (r.payload.length === 0) { this.emit("ready"); continue; }
      this.emit("data", r.channel, r.tick, r.payload);
      if (this.joined?.mode === "relay" || (r.payload[0] & 0xf0) !== 0x10 && (r.payload[0] & 0xf0) !== 0x20) {
        const f = decodeEvent(r.payload); if (f) this.emit("event", f);
      }
    }
  }

  private async openPath(d: Joined["data"]): Promise<DataPath> {
    const WT: any = (globalThis as any).WebTransport;
    if (WT && d.webtransport_url && !this.forceTunnel) {
      try {
        const hash = Uint8Array.from(d.cert_sha256.match(/../g)!.map((h) => parseInt(h, 16)));
        const wt = new WT(d.webtransport_url, { serverCertificateHashes: [{ algorithm: "sha-256", value: hash }] });
        await wt.ready;
        const writer = wt.datagrams.writable.getWriter();
        const reader = wt.datagrams.readable.getReader();
        (async () => { try { for (;;) { const { value, done } = await reader.read(); if (done) break; this.onDatagram(value); } } catch { /* closed */ } })();
        wt.closed.then(() => { this.stop(); this.emit("closed", "webtransport closed"); }, () => { this.stop(); this.emit("closed", "webtransport error"); });
        return { kind: "webtransport", send: (b) => { writer.write(b).catch(() => {}); }, close: () => wt.close() };
      } catch (e) {
        // fall through to the tunnel
      }
    }
    if (!d.ws_tunnel) throw new Error("no usable data path (webtransport failed, tunnel disabled)");
    return { kind: "ws-tunnel", send: (b) => this.ws.send(b), close: () => {} };
  }
  private flushLink() { for (const d of this.link.drain()) this.path?.send(d); }

  /** Queue a data message; unreliable ones leave on flush()/timer. */
  sendData(channel: Channel, payload: Uint8Array) { this.link.send(channel, payload); if (channel === Channel.Reliable) this.flushLink(); }
  sendEvent(code: number, payload: Uint8Array, target: Target = "others", reliable = false) { this.sendData(reliable ? Channel.Reliable : Channel.Unreliable, encodeEvent(code, payload, target)); }
  /** Push queued unreliable messages now (call at end of frame). */
  flush() { this.link.flushBatch(); this.flushLink(); }
  get stats() { return this.link.stats; }

  leave() { this.send({ t: "leave" }); }
  close() { this.stop(); this.path?.close(); this.ws.close(); }
  private stop() { if (this.timer) { clearInterval(this.timer); this.timer = undefined; } }
}
