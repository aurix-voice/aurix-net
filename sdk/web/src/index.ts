import { Channel } from "./packet.js";
import { Link, type Received } from "./link.js";
import {
  encodeEvent,
  decodeEvent,
  type Target,
  type CacheOp,
  type EventFrame,
} from "./event.js";
export { Channel } from "./packet.js";
export { Link } from "./link.js";
export { encodeEvent, decodeEvent } from "./event.js";
export type { Target, EventFrame } from "./event.js";
export * from "./sync.js";
export * from "./lockstep.js";
export * from "./chat.js";

export type RoomMode = "relay" | "sync" | "lockstep";
/** `stats` answer: live rooms/players of the project, grouped by the room prop `lobby` ("" = default lobby). */
export interface Stats {
  rooms: number;
  players: number;
  lobbies: { lobby: string; rooms: number; players: number }[];
}
/** `get_rooms` / lobby entry. */
export interface RoomSummary {
  id: string;
  name: string;
  mode: string;
  players: number;
  max_players: number;
  open: boolean;
  props: Record<string, unknown>;
}
/** `rpc` answer: `ok` = your endpoint answered 2xx; `body` is its JSON (null when empty); `error` explains a failed call. */
export interface RpcResult {
  ok: boolean;
  status?: number;
  body: unknown;
  error?: string;
}
/** `find_friends` answer: online = authenticated somewhere in the fleet or holding a room slot. */
export interface Friend {
  player: string;
  online: boolean;
  room?: { id: string; name: string; mode: RoomMode; node: string };
}
export interface Joined {
  room: string;
  name: string;
  mode: RoomMode;
  actor: number;
  master: number;
  tick_hz?: number;
  schema_hash?: string;
  /** Relay events the room kept for late joiners: `[code, from, base64 payload]`, oldest first. */
  cached?: [number, number, string][];
  data: {
    host: string;
    port: number;
    cert_sha256: string;
    ticket: string;
    ws_tunnel: boolean;
    webtransport_url?: string;
  };
}

/** Data path: WebTransport datagrams when the browser and the node support it, else the WS tunnel. */
interface DataPath {
  send(bytes: Uint8Array): void;
  close(): void;
  kind: "webtransport" | "ws-tunnel";
}

/** A control message from the node: JSON tagged by `t`; the other fields depend on the tag. */
export interface ControlMessage {
  t: string;
  [field: string]: unknown;
}

/** What `Client.on` can subscribe to, with the arguments each listener receives. */
export interface ClientEvents {
  control: (message: ControlMessage) => void;
  data: (channel: Channel, tick: number, payload: Uint8Array) => void;
  event: (frame: EventFrame) => void;
  ready: () => void;
  closed: (reason: string) => void;
  rehost: () => void;
  notice: (text: string, level: string) => void;
}

export interface RegionCandidate {
  region: string;
  displayName: string;
  endpoint: string;
  rttMs: number;
}
export interface RegionPick {
  region: string;
  endpoint: string;
  rttMs: number;
  candidates: RegionCandidate[];
}

/** Browser/Node client. Control over WebSocket; data over WebTransport datagrams (browsers)
 *  or the WebSocket tunnel (fallback / Node). Set `forceTunnel` to skip WebTransport. */
export class Client {
  private ws!: WebSocket;
  private link = new Link();
  private timer?: ReturnType<typeof setInterval>;
  private path?: DataPath;
  private onReady?: () => void;
  private isLost = false;
  forceTunnel = false;
  /** Which data path is in use after join(). */
  get dataPath() {
    return this.path?.kind;
  }
  private listeners: { [K in keyof ClientEvents]?: ClientEvents[K][] } = {};
  private waiters: ((message: ControlMessage) => boolean)[] = [];
  private rpcSeq = 0;
  joined?: Joined;
  actor = -1;

  on<K extends keyof ClientEvents>(event: K, listener: ClientEvents[K]) {
    const listeners: ClientEvents[K][] = (this.listeners[event] ??= []);
    listeners.push(listener);
    return this;
  }
  private emit<K extends keyof ClientEvents>(
    event: K,
    ...args: Parameters<ClientEvents[K]>
  ) {
    const listeners = (this.listeners[event] ?? []) as ((
      ...parameters: Parameters<ClientEvents[K]>
    ) => void)[];
    for (const listener of listeners) {
      listener(...args);
    }
  }

  /** URL given to connect(), the grant and the last join message: what rejoin() needs. */
  entryUrl = "";
  grant = "";
  lastJoin: object | null = null;
  /** Events the room kept for late joiners, as received by the last join; also emitted as `event`. */
  cached: EventFrame[] = [];
  private opts: { timeoutMs?: number } = {};
  /** Region list and best-region ping: reads `/v1/regions` from the catalog (or any node), probes every
   *  live endpoint's `/healthz` and returns the fastest; pass `endpoint` to `connect`. */
  static async bestRegion(
    catalogHttp: string,
    probes = 3,
  ): Promise<RegionPick> {
    const base = catalogHttp.replace(/\/$/, "");
    const regions = (await (
      await fetch(`${base}/v1/regions`)
    ).json()) as Array<{
      id: string;
      display_name: string;
      endpoints: string[];
    }>;
    const now = () =>
      typeof performance !== "undefined" ? performance.now() : Date.now();
    const candidates = await Promise.all(
      regions.flatMap((region) =>
        region.endpoints.map(async (ep) => {
          const endpoint = ep.replace(/\/$/, "");
          let best = -1;
          for (let i = 0; i < Math.max(1, Math.min(10, probes)); i++) {
            const start = now();
            try {
              await fetch(`${endpoint}/healthz`, { cache: "no-store" });
              const ms = Math.round(now() - start);
              best = best < 0 ? ms : Math.min(best, ms);
            } catch {
              /* unreachable */
            }
          }
          return {
            region: region.id,
            displayName: region.display_name,
            endpoint,
            rttMs: best,
          };
        }),
      ),
    );
    const ok = candidates
      .filter((char) => char.rttMs >= 0)
      .sort((left, right) => left.rttMs - right.rttMs);
    if (!ok.length) {
      throw new Error("no reachable region");
    }
    return {
      region: ok[0].region,
      endpoint: ok[0].endpoint,
      rttMs: ok[0].rttMs,
      candidates,
    };
  }

  static async connect(
    httpUrl: string,
    grant: string,
    opts: { timeoutMs?: number } = {},
  ): Promise<Client> {
    const client = new Client();
    client.entryUrl = httpUrl;
    client.grant = grant;
    client.opts = opts;
    const url = httpUrl.replace(/^http/, "ws") + "/ws";
    await new Promise<void>((res, rej) => {
      const timer = setTimeout(
        () => rej(new Error("timeout")),
        opts.timeoutMs ?? 10000,
      );
      client.ws = new WebSocket(url);
      client.ws.binaryType = "arraybuffer";
      client.ws.onopen = () => {
        clearTimeout(timer);
        res();
      };
      client.ws.onerror = () => {
        clearTimeout(timer);
        rej(new Error("websocket error"));
      };
    });
    client.ws.onmessage = (event) => client.onMessage(event.data);
    client.ws.onclose = () => client.lost("control channel closed");
    client.send({ t: "auth", grant });
    await client.waitFor(
      (message) => message.t === "authed",
      (message) => message.t === "error",
    );
    return client;
  }

  private onMessage(data: unknown) {
    if (typeof data === "string") {
      const message = JSON.parse(data) as ControlMessage;
      this.emit("control", message);
      if (message.t === "left" && message.reason === "rehost") {
        this.emit("rehost");
      }
      if (message.t === "notice") {
        this.emit(
          "notice",
          typeof message.text === "string" ? message.text : "",
          typeof message.level === "string" ? message.level : "info",
        );
      }
      this.waiters = this.waiters.filter((matches) => !matches(message));
    } else {
      this.onDatagram(new Uint8Array(data as ArrayBuffer));
    }
  }
  private waitFor<T = ControlMessage>(
    ok: (message: ControlMessage) => boolean,
    fail?: (message: ControlMessage) => boolean,
    timeoutMs = 10000,
  ): Promise<T> {
    return new Promise((res, rej) => {
      const timer = setTimeout(() => rej(new Error("timeout")), timeoutMs);
      this.waiters.push((message) => {
        if (ok(message)) {
          clearTimeout(timer);
          res(message as T);
          return true;
        }
        if (fail?.(message)) {
          clearTimeout(timer);
          rej(
            Object.assign(new Error(String(message.message ?? message.code)), {
              code: message.code,
            }),
          );
          return true;
        }
        return false;
      });
    });
  }

  /** Any control message, e.g. {t:"set_room_props", props:{...}}. */
  send(msg: object) {
    this.ws.send(JSON.stringify(msg));
  }

  /** connect + join, following one `room_elsewhere:<url>` redirect to the node hosting the room. */
  static async connectAndJoin(
    httpUrl: string,
    grant: string,
    msg: object,
    opts: { timeoutMs?: number } = {},
  ): Promise<{ client: Client; joined: Joined }> {
    try {
      return await Client.joinOrClose(
        await Client.connect(httpUrl, grant, opts),
        msg,
      );
    } catch (error) {
      const text = String((error as Error).message ?? error);
      const index = text.indexOf("room_elsewhere:");
      if (index < 0) {
        throw error;
      }
      const url = text.slice(index + "room_elsewhere:".length).trim();
      return Client.joinOrClose(await Client.connect(url, grant, opts), msg);
    }
  }
  private static async joinOrClose(
    client: Client,
    msg: object,
  ): Promise<{ client: Client; joined: Joined }> {
    try {
      return { client, joined: await client.join(msg) };
    } catch (error) {
      client.close();
      throw error;
    }
  }
  /** `reserved`: player ids whose seats are held for 60 s when this call creates the room (a party
   *  arriving together: pass the `players` list from a matchmaking ticket). */
  async joinOrCreate(
    name: string,
    mode: RoomMode = "relay",
    maxPlayers = 8,
    props: Record<string, unknown> = {},
    reserved: string[] = [],
  ): Promise<Joined> {
    return this.join({
      t: "join_or_create",
      name,
      mode,
      max_players: maxPlayers,
      props,
      reserved,
    });
  }
  /** Joins a random open room: `filter` must equal room props exactly, `expr` is a SQL-like condition
   *  on the room's lobby props (`map = 'dust' AND skill <= 1500`), `matchType` picks among candidates
   *  (`fill` fullest first, `serial` emptiest first, `random`). Follows a redirect to the hosting node. */
  async joinRandom(
    filter: Record<string, unknown> = {},
    expr?: string,
    matchType: "fill" | "serial" | "random" = "fill",
  ): Promise<Joined> {
    return this.join({ t: "join_random", filter, expr, match_type: matchType });
  }
  /** After `left{reason:"rehost"}` (the node shut down and handed the room to another node): a fresh
   *  client in the same room with the same grant. Tries the entry URL, then `fallbackUrls`. */
  async rejoin(
    fallbackUrls: string[] = [],
  ): Promise<{ client: Client; joined: Joined }> {
    if (!this.lastJoin) {
      throw new Error("rejoin: never joined");
    }
    const msg = { ...(this.lastJoin as Record<string, unknown>) };
    if (msg.t === "join_or_create") {
      msg.t = "join";
    }
    this.close();
    let err: unknown = null;
    for (const url of [this.entryUrl, ...fallbackUrls]) {
      try {
        return await Client.connectAndJoin(url, this.grant, msg, this.opts);
      } catch (error) {
        err = error;
      }
    }
    throw err;
  }
  async join(msg: object): Promise<Joined> {
    this.lastJoin = msg;
    this.send(msg);
    const joined = await this.waitFor<Joined>(
      (message) => message.t === "joined",
      (message) => message.t === "error",
    );
    this.joined = joined;
    this.actor = joined.actor;
    // open the data path: ticket as the first datagram/frame, wait for the server's empty ack
    const ticket = Uint8Array.from(atob(joined.data.ticket), (char) =>
      char.charCodeAt(0),
    );
    // every join gets its own data path, and the node starts a fresh link on it
    this.stop();
    const old = this.path;
    this.path = undefined;
    old?.close();
    this.link = new Link();
    this.isLost = false;
    const ready = new Promise<void>((res, rej) => {
      const timer = setTimeout(() => {
        this.onReady = undefined;
        rej(new Error("data path timeout"));
      }, 10000);
      this.onReady = () => {
        clearTimeout(timer);
        res();
      };
    });
    this.path = await this.openPath(joined.data);
    this.path.send(ticket);
    this.timer = setInterval(() => {
      if (!this.link.poll()) {
        this.lost("link dead");
      }
      this.flushLink();
    }, 5);
    await ready;
    this.cached = (joined.cached ?? []).map(([code, from, data]) => ({
      code,
      from,
      target: "others",
      payload: Uint8Array.from(atob(data), (char) => char.charCodeAt(0)),
      cached: true,
    }));
    for (const frame of this.cached) {
      this.emit("event", frame);
    }
    return joined;
  }

  private onDatagram(bytes: Uint8Array) {
    const recv: Received[] = this.link.receive(bytes);
    this.flushLink();
    for (const received of recv) {
      if (received.payload.length === 0) {
        const ready = this.onReady;
        this.onReady = undefined;
        ready?.();
        this.emit("ready");
        continue;
      }
      this.emit("data", received.channel, received.tick, received.payload);
      if (
        this.joined?.mode === "relay" ||
        ((received.payload[0] & 0xf0) !== 0x10 &&
          (received.payload[0] & 0xf0) !== 0x20)
      ) {
        const frame = decodeEvent(received.payload);
        if (frame) {
          this.emit("event", frame);
        }
      }
    }
  }

  private async openPath(data: Joined["data"]): Promise<DataPath> {
    const WT = (globalThis as { WebTransport?: typeof WebTransport })
      .WebTransport;
    if (WT && data.webtransport_url && !this.forceTunnel) {
      try {
        const hash = Uint8Array.from(
          data.cert_sha256.match(/../g)!.map((hex) => parseInt(hex, 16)),
        );
        const wt = new WT(data.webtransport_url, {
          serverCertificateHashes: [{ algorithm: "sha-256", value: hash }],
        });
        await wt.ready;
        const writer = wt.datagrams.writable.getWriter();
        const reader = (
          wt.datagrams.readable as ReadableStream<Uint8Array>
        ).getReader();
        void (async () => {
          try {
            for (;;) {
              const { value, done } = await reader.read();
              if (done) {
                break;
              }
              this.onDatagram(value);
            }
          } catch {
            /* closed */
          }
        })();
        const path: DataPath = {
          kind: "webtransport",
          send: (bytes) => {
            writer.write(bytes).catch(() => {});
          },
          close: () => wt.close(),
        };
        const gone = (why: string) => {
          if (this.path === path) {
            this.lost(why);
          }
        };
        wt.closed.then(
          () => gone("webtransport closed"),
          () => gone("webtransport error"),
        );
        return path;
      } catch {
        // fall through to the tunnel
      }
    }
    if (!data.ws_tunnel) {
      throw new Error(
        "no usable data path (webtransport failed, tunnel disabled)",
      );
    }
    return {
      kind: "ws-tunnel",
      send: (bytes) => this.ws.send(bytes),
      close: () => {},
    };
  }
  private flushLink() {
    for (const bytes of this.link.drain()) {
      this.path?.send(bytes);
    }
  }

  /** Queue a data message; unreliable ones leave on flush()/timer. */
  sendData(channel: Channel, payload: Uint8Array) {
    this.link.send(channel, payload);
    if (channel === Channel.Reliable) {
      this.flushLink();
    }
  }
  /** Relay event; `opts.cache` keeps it for late joiners (see `cacheSlice`), `opts.group` sends to one interest group. */
  sendEvent(
    code: number,
    payload: Uint8Array,
    target: Target = "others",
    reliable = false,
    opts: { group?: number; cache?: CacheOp } = {},
  ) {
    this.sendData(
      reliable || opts.cache ? Channel.Reliable : Channel.Unreliable,
      encodeEvent(code, payload, target, opts.group, opts.cache),
    );
  }
  /** Relay event-cache slices: `increase` opens the next slice, `set` picks one, `purge` drops one slice, `purge_up_to` every slice up to `index`. */
  cacheSlice(op: "increase" | "set" | "purge" | "purge_up_to", index?: number) {
    this.send({ t: "cache_slice", op, index });
  }
  /** Push queued unreliable messages now (call at end of frame). */
  flush() {
    this.link.flushBatch();
    this.flushLink();
  }
  get stats() {
    return this.link.stats;
  }

  leave() {
    this.send({ t: "leave" });
  }
  /** Project-wide room/player counts per lobby (lobby statistics); at most one per second. */
  async lobbyStats(): Promise<Stats> {
    this.send({ t: "stats" });
    const reply = await this.waitFor<Stats>(
      (message) => message.t === "stats",
      (message) => message.t === "error",
    );
    return {
      rooms: reply.rooms,
      players: reply.players,
      lobbies: reply.lobbies,
    };
  }
  /** Presence of up to 64 players (find friends): online and the room they are in, fleet-wide. */
  async findFriends(players: string[]): Promise<Friend[]> {
    this.send({ t: "find_friends", players });
    const reply = await this.waitFor<{ friends: Friend[] }>(
      (message) => message.t === "friends",
      (message) => message.t === "error",
    );
    return reply.friends;
  }
  /** One-shot room list (SQL-lobby style): open, visible rooms with a free slot, optionally filtered by a SQL-like `expr` on lobby props (`map = 'dust' AND rank_min <= 1400`); one per second. */
  async rooms(expr?: string, limit?: number): Promise<RoomSummary[]> {
    this.send({ t: "get_rooms", expr, limit });
    const reply = await this.waitFor<{ rooms: RoomSummary[] }>(
      (message) => message.t === "rooms",
      (message) => message.t === "error",
    );
    return reply.rooms;
  }
  /** Call your backend through the node (WebRPC): the project's webhook subscribed to `rpc` is POSTed `{name, player_id, actor?, room?, body}` and its JSON answer comes back. Body ≤ 8 KiB, 5 calls/s. */
  async rpc(name: string, body: unknown = null): Promise<RpcResult> {
    const id = (this.rpcSeq = (this.rpcSeq + 1) >>> 0);
    this.send({ t: "rpc", id, name, body });
    const reply = await this.waitFor<RpcResult>(
      (message) => message.t === "rpc_result" && message.id === id,
      (message) => message.t === "error",
    );
    return {
      ok: reply.ok,
      status: reply.status,
      body: reply.body,
      error: reply.error,
    };
  }
  /** Relay interest groups: events raised with `group` reach only its members. */
  setGroups(join: number[] = [], leave: number[] = []) {
    this.send({ t: "set_groups", join, leave });
  }
  /** Sync rooms: steer area-of-interest: extra viewpoints (camera, scope target) and entities forced into view (team mates, objectives). Entity ids as in deltas. */
  setView(
    viewpoints: (number | bigint)[] = [],
    interest: (number | bigint)[] = [],
  ) {
    this.send({
      t: "set_view",
      viewpoints: viewpoints.map(Number),
      interest: interest.map(Number),
    });
  }
  close() {
    this.stop();
    this.path?.close();
    this.ws.close();
  }
  // One lost connection is reported once, whichever of the link, the data path and the control channel
  // notices it first, and closing the client afterwards does not report it again.
  private lost(why: string) {
    if (this.isLost) {
      return;
    }
    this.isLost = true;
    this.stop();
    this.emit("closed", why);
  }
  private stop() {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = undefined;
    }
  }
}

export * from "./sim.js";
export * from "./bots.js";
