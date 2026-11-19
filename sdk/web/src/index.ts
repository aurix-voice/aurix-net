import { Channel } from "./packet.js";
import { Link, LEAVE_WAIT, type Received } from "./link.js";
import { report } from "./report.js";
import {
  encodeEvent,
  decodeEvent,
  decodeSentEvent,
  type Target,
  type CacheOp,
  type EventFrame,
  type UndeliveredEvent,
} from "./event.js";
export { Channel } from "./packet.js";
export {
  Link,
  IDLE_TIMEOUT,
  LEAVE_WAIT,
  RELIABLE_BACKLOG_BYTES,
} from "./link.js";
export { encodeEvent, decodeEvent } from "./event.js";
export type { Target, EventFrame, UndeliveredEvent } from "./event.js";
export * from "./sync.js";
export * from "./lockstep.js";
export * from "./chat.js";

const TICKET_RESEND_MS = 100;
// The node closes a control channel it has heard nothing from for 90 s.
const PING_EVERY_MS = 1000;
const PROBES_KEPT = 16;

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
  /** Lockstep rooms: the seed every client and the node start the game from. */
  seed?: number;
  /** Sync rooms that answer tagged spawns: the first tag the room has not seen from this actor. */
  next_spawn_tag?: number;
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

// What this client names in `auth`, so the node sends it nothing it does not know.
const SUPPORTS = ["unsequenced"];

// The join that brings a client back into the room `asked` led to: the room by its name, as the
// player that entered it. A random join or a join_or_create asked again could lead elsewhere.
function wayBack(asked: Record<string, unknown>, name: string): object {
  const kept =
    asked.t === "join"
      ? ["player_props", "schema_hash", "spectator"]
      : ["player_props"];
  const again: Record<string, unknown> = { t: "join", name };
  for (const key of kept) {
    if (asked[key] !== undefined) {
      again[key] = asked[key];
    }
  }
  return again;
}

// The mark of an event among the messages of the link.
const EVENT = 1;

// An `error` refuses the request whose `id` it repeats; one without an `id` may refuse any.
function refuses(message: ControlMessage, id?: number): boolean {
  return (
    message.t === "error" && (message.id === undefined || message.id === id)
  );
}

/** What `Client.on` can subscribe to, with the arguments each listener receives. */
export interface ClientEvents {
  control: (message: ControlMessage) => void;
  data: (channel: Channel, tick: number, payload: Uint8Array) => void;
  event: (frame: EventFrame) => void;
  ready: () => void;
  closed: (reason: string) => void;
  /** The reliable events the node had not acknowledged when the room was lost, in the order
   *  they were sent. Comes before `closed` and before the `left` of a room the node took the
   *  client out of or `leave` left without their acknowledgement. The room may have got any
   *  of them. */
  undelivered: (events: UndeliveredEvent[]) => void;
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
  private nodeSupports = new Set<string>();
  private timer?: ReturnType<typeof setInterval>;
  private path?: DataPath;
  private onReady?: () => void;
  private failReady?: (error: Error) => void;
  private isLost = false;
  private ended?: Error;
  /** The leave that waits for the node's acknowledgements, and what tells it of a change. */
  private leaving?: Promise<void>;
  private acknowledged?: (late?: boolean) => void;
  /** True once the data path of the room in `joined` is up: until then a join is entering it. */
  private entered = false;
  /** With what a join fails that the node ended before the join got to open the data path. */
  private turnedOut?: Error;
  forceTunnel = false;
  /** Which data path is in use after join(). */
  get dataPath() {
    return this.path?.kind;
  }
  private listeners: { [K in keyof ClientEvents]?: ClientEvents[K][] } = {};
  private waiters: {
    take: (message: ControlMessage) => boolean;
    fail: (error: Error) => void;
  }[] = [];
  private requestSeq = 0;
  joined?: Joined;
  actor = -1;
  /** The player id of the grant this client signed in with: the id the node knows the player by
   *  in rooms, held seats and webhooks. */
  playerId = "";
  /** The display name of the grant this client signed in with. */
  playerName = "";
  /** Sync rooms: the entity the room made for this client as its avatar, once the room has
   *  announced it. Kept from the join on, whether or not anything listens to `data`. */
  avatar?: number;

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
      // as with an EventTarget: a listener that throws stops neither the others nor the client
      try {
        listener(...args);
      } catch (error) {
        report(error);
      }
    }
  }

  /** URL given to connect(), the grant and the join that leads back into the last room: what
   *  rejoin() needs. */
  entryUrl = "";
  grant = "";
  lastJoin: object | null = null;
  /** Events the room kept for late joiners, as received by the last join; also emitted as `event`. */
  cached: EventFrame[] = [];
  /** Smoothed control round trip in milliseconds; 0 until the first probe got its answer. */
  rttMs = 0;
  private probe = -1;
  private probes = new Set<number>();
  private keepalive?: ReturnType<typeof setInterval>;
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
    client.ws.onclose = () => client.controlClosed();
    client.send({ t: "auth", grant, supports: SUPPORTS });
    try {
      const authed = await client.waitFor<{ player_id: string; name: string }>(
        (message) => message.t === "authed",
        refuses,
      );
      client.playerId = authed.player_id;
      client.playerName = authed.name;
      client.ping();
      client.keepalive = setInterval(() => client.ping(), PING_EVERY_MS);
    } catch (error) {
      client.close();
      throw error;
    }
    return client;
  }

  private onMessage(data: unknown) {
    if (typeof data === "string") {
      const message = JSON.parse(data) as ControlMessage;
      if (message.t === "pong" && this.probes.delete(message.ts as number)) {
        const sample = Math.max(0, performance.now() - (message.ts as number));
        this.rttMs = this.rttMs ? 0.875 * this.rttMs + 0.125 * sample : sample;
        return;
      }
      if (message.t === "hello" && Array.isArray(message.supports)) {
        this.nodeSupports = new Set(message.supports as string[]);
      }
      if (
        message.t === "left" &&
        message.reason !== "left" &&
        this.joined &&
        message.room === this.joined.room
      ) {
        const entered = this.entered;
        const events = this.unacknowledgedEvents();
        this.closePath();
        this.forgetRoom();
        if (!entered) {
          // the join fails with the reason, and the game hears of no room it was never in
          const error = Object.assign(new Error(String(message.reason)), {
            code: "left",
          });
          if (this.failReady) {
            this.failReady(error);
          } else {
            this.turnedOut = error;
          }
          return;
        }
        this.handBack(events);
      }
      this.emit("control", message);
      if (message.t === "error" && message.code === "transport") {
        // the node gave the data path up and will take nothing more over it
        this.lost("link dead");
      }
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
      this.waiters = this.waiters.filter((waiter) => !waiter.take(message));
    } else {
      this.onDatagram(new Uint8Array(data as ArrayBuffer));
    }
  }
  // A sync room announces the avatar as the answer (0x14) to spawn tag 0.
  private noteAvatar(frame: Uint8Array) {
    if (frame.length !== 18 || frame[0] !== 0x14) {
      return;
    }
    const view = new DataView(frame.buffer, frame.byteOffset);
    if (view.getUint32(1) === 0 && view.getUint8(5) === 0) {
      this.avatar = Number(view.getBigUint64(6));
    }
  }
  /** The client is out of its room: by `leave`, or the node took it out (kick, room closed). */
  private forgetRoom() {
    this.joined = undefined;
    this.entered = false;
    this.actor = -1;
    this.avatar = undefined;
  }

  private waitFor<T = ControlMessage>(
    ok: (message: ControlMessage) => boolean,
    fail?: (message: ControlMessage) => boolean,
    timeoutMs = 10000,
  ): Promise<T> {
    if (this.ended) {
      return Promise.reject(this.ended);
    }
    return new Promise((res, rej) => {
      const timer = setTimeout(() => rej(new Error("timeout")), timeoutMs);
      this.waiters.push({
        take: (message) => {
          if (ok(message)) {
            clearTimeout(timer);
            res(message as T);
            return true;
          }
          if (fail?.(message)) {
            clearTimeout(timer);
            rej(
              Object.assign(
                new Error(String(message.message ?? message.code)),
                { code: message.code },
              ),
            );
            return true;
          }
          return false;
        },
        fail: (error) => {
          clearTimeout(timer);
          rej(error);
        },
      });
    });
  }

  /** Any control message, e.g. {t:"set_room_props", props:{...}}. */
  send(msg: object) {
    this.ws.send(JSON.stringify(msg));
  }

  /** Measures the control round trip into `rttMs`. The client does it every second by itself,
   *  which also keeps the node from closing a control channel it hears nothing from. The answer is
   *  not emitted as `control`. */
  ping(): void {
    // no two probes in flight carry the same time, so each answer is told from a game's own ping
    this.probe = Math.max(Math.floor(performance.now()), this.probe + 1);
    this.probes.add(this.probe);
    if (this.probes.size > PROBES_KEPT) {
      this.probes.delete(this.probes.values().next().value as number);
    }
    this.send({ t: "ping", ts: this.probe });
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
   *  arriving together: pass the `players` list from a matchmaking ticket). `lobbyProps`: names of
   *  the room props that room listings show and `expr` filters match. `visible: false` keeps the
   *  room out of room listings and random joins, `open: false` lets nobody else in. All of these
   *  shape the room only when this call creates it. */
  async joinOrCreate(
    name: string,
    mode: RoomMode = "relay",
    maxPlayers = 8,
    props: Record<string, unknown> = {},
    reserved: string[] = [],
    lobbyProps: string[] = [],
    visible = true,
    open = true,
  ): Promise<Joined> {
    return this.join({
      t: "join_or_create",
      name,
      mode,
      max_players: maxPlayers,
      visible,
      open,
      props,
      reserved,
      lobby_props: lobbyProps,
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
    const msg = this.lastJoin;
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
    if (this.leaving) {
      await this.leaving;
    }
    // a node that repeats the id of a join answers this one and no other with it
    const id = this.joinId();
    this.send(id === undefined ? msg : { ...msg, id });
    let joined: Joined;
    try {
      joined = await this.waitFor<Joined>(
        (reply) => {
          if (reply.t !== "joined" || reply.id !== id) {
            return false;
          }
          // a `left` of this room that follows is the end of this join
          this.joined = reply as unknown as Joined;
          this.entered = false;
          this.turnedOut = undefined;
          return true;
        },
        (reply) => refuses(reply, id),
      );
    } catch (error) {
      if (
        (error as { code?: string }).code === undefined &&
        this.ws.readyState === this.ws.OPEN
      ) {
        // the node may have let the client in, or may yet: the client is in no room after this
        this.leaveNow();
      }
      throw error;
    }
    if (this.turnedOut) {
      throw this.turnedOut;
    }
    this.lastJoin = wayBack(msg as Record<string, unknown>, joined.name);
    this.actor = joined.actor;
    this.avatar = undefined;
    // open the data path: ticket as the first datagram/frame, wait for the server's empty ack
    const ticket = Uint8Array.from(atob(joined.data.ticket), (char) =>
      char.charCodeAt(0),
    );
    // every join gets its own data path, and the node starts a fresh link on it
    this.closePath();
    this.isLost = false;
    const ready = new Promise<void>((res, rej) => {
      const timer = setTimeout(
        () => this.failReady?.(new Error("data path timeout")),
        10000,
      );
      const settle = () => {
        clearTimeout(timer);
        this.onReady = undefined;
        this.failReady = undefined;
      };
      this.onReady = () => {
        settle();
        this.entered = true;
        res();
      };
      this.failReady = (error) => {
        settle();
        rej(error);
      };
    });
    // it may fail while the path is still opening, before anything awaits it
    ready.catch(() => {});
    this.path = await this.openPath(joined.data, ticket);
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
    if (!this.path) {
      return;
    }
    const recv: Received[] = this.link.receive(bytes);
    this.flushLink();
    this.acknowledged?.();
    for (const received of recv) {
      if (received.payload.length === 0) {
        const ready = this.onReady;
        this.onReady = undefined;
        ready?.();
        this.emit("ready");
        continue;
      }
      this.noteAvatar(received.payload);
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

  private async openPath(
    data: Joined["data"],
    ticket: Uint8Array,
  ): Promise<DataPath> {
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
        const send = (bytes: Uint8Array) => {
          writer.write(bytes).catch(() => {});
        };
        send(ticket);
        // the ticket is a datagram: it is repeated until the node's first one arrives
        const again = setInterval(() => send(ticket), TICKET_RESEND_MS);
        void (async () => {
          for (;;) {
            const read = await reader.read().catch(() => undefined);
            if (!read || read.done) {
              break;
            }
            clearInterval(again);
            this.onDatagram(read.value);
          }
        })();
        const path: DataPath = {
          kind: "webtransport",
          send,
          close: () => {
            clearInterval(again);
            wt.close();
          },
        };
        const gone = (why: string) => {
          clearInterval(again);
          if (this.path === path) {
            this.lost(why);
          }
        };
        wt.closed.then(
          // the node closes the session with code 4 when it gave the link up
          (info) =>
            gone(info?.closeCode === 4 ? "link dead" : "webtransport closed"),
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
    this.ws.send(ticket);
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

  /** Queue a data message; unreliable ones leave on flush()/timer. A node that does not know the
   *  unsequenced channel gets its messages on the sequenced one. Throws outside a room and once
   *  the connection is lost. It is lost when the node acknowledges nothing for `IDLE_TIMEOUT`
   *  and when more than `RELIABLE_BACKLOG_BYTES` of reliable messages wait for it. */
  sendData(channel: Channel, payload: Uint8Array) {
    this.queue(channel, payload, 0);
  }
  private queue(channel: Channel, payload: Uint8Array, mark: number) {
    if (!this.path || this.isLost || this.leaving) {
      throw new Error("disconnected");
    }
    if (
      channel === Channel.UnreliableUnsequenced &&
      !this.nodeSupports.has("unsequenced")
    ) {
      channel = Channel.Unreliable;
    }
    try {
      this.link.send(channel, payload, undefined, mark);
    } catch (error) {
      if (this.link.isDead) {
        this.lost("link dead");
      }
      throw error;
    }
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
    this.sendEventOn(
      reliable || opts.cache ? Channel.Reliable : Channel.Unreliable,
      code,
      payload,
      target,
      opts,
    );
  }
  /** `sendEvent` on a channel of the caller's choice: `Channel.Unreliable` for state, where a newer
   *  event replaces an older one, `Channel.UnreliableUnsequenced` for events that are all wanted. */
  sendEventOn(
    channel: Channel,
    code: number,
    payload: Uint8Array,
    target: Target = "others",
    opts: { group?: number; cache?: CacheOp } = {},
  ) {
    this.queue(
      channel,
      encodeEvent(code, payload, target, opts.group, opts.cache),
      EVENT,
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

  /** Leaves the room once the node has acknowledged what the game sent it reliably, or after
   *  `LEAVE_WAIT`: the events it has not acknowledged by then come as `undelivered` before the
   *  `left`. Until then the client stays in the room and takes no new sends. Resolves when the
   *  client is out of the room. */
  leave(): Promise<void> {
    if (this.leaving) {
      return this.leaving;
    }
    const path = this.path;
    if (!path || !this.link.unacknowledged().length) {
      this.leaveNow();
      return Promise.resolve();
    }
    this.leaving = new Promise<void>((resolve) => {
      const end = setTimeout(() => this.acknowledged?.(true), LEAVE_WAIT);
      this.acknowledged = (late = false) => {
        if (!late && this.path === path && this.link.unacknowledged().length) {
          return;
        }
        clearTimeout(end);
        this.acknowledged = undefined;
        this.leaving = undefined;
        // a path that is gone took the room with it: the node took the client out, or the
        // connection is lost
        if (this.path === path) {
          const events = this.unacknowledgedEvents();
          this.leaveNow();
          this.handBack(events);
        }
        resolve();
      };
    });
    return this.leaving;
  }
  private leaveNow() {
    this.closePath();
    this.forgetRoom();
    this.send({ t: "leave", id: this.joinId() });
  }
  private joinId(): number | undefined {
    return this.nodeSupports.has("join_id") ? this.nextId() : undefined;
  }
  private nextId(): number {
    return (this.requestSeq = (this.requestSeq + 1) >>> 0);
  }
  /** Sends `message` under an id of its own and waits for the `answer` or the `error` that
   *  repeats it. A node that repeats no ids answers without one, and that answer is taken. */
  private request<T>(message: object, answer: string): Promise<T> {
    const id = this.nextId();
    this.send({ ...message, id });
    return this.waitFor<T>(
      (reply) =>
        reply.t === answer && (reply.id === undefined || reply.id === id),
      (reply) => refuses(reply, id),
    );
  }
  /** Project-wide room/player counts per lobby (lobby statistics); at most one per second. */
  async lobbyStats(): Promise<Stats> {
    const reply = await this.request<Stats>({ t: "stats" }, "stats");
    return {
      rooms: reply.rooms,
      players: reply.players,
      lobbies: reply.lobbies,
    };
  }
  /** Presence of up to 64 players (find friends): online and the room they are in, fleet-wide. */
  async findFriends(players: string[]): Promise<Friend[]> {
    const reply = await this.request<{ friends: Friend[] }>(
      { t: "find_friends", players },
      "friends",
    );
    return reply.friends;
  }
  /** One-shot room list (SQL-lobby style): open, visible rooms with a free slot, optionally filtered by a SQL-like `expr` on lobby props (`map = 'dust' AND rank_min <= 1400`); one per second. */
  async rooms(expr?: string, limit?: number): Promise<RoomSummary[]> {
    const reply = await this.request<{ rooms: RoomSummary[] }>(
      { t: "get_rooms", expr, limit },
      "rooms",
    );
    return reply.rooms;
  }
  /** Call your backend through the node (WebRPC): the project's webhook subscribed to `rpc` is POSTed `{name, player_id, actor?, room?, body}` and its JSON answer comes back. Body ≤ 8 KiB, 5 calls/s. */
  async rpc(name: string, body: unknown = null): Promise<RpcResult> {
    const reply = await this.request<RpcResult>(
      { t: "rpc", name, body },
      "rpc_result",
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
    clearInterval(this.keepalive);
    this.closePath();
    this.ws.close();
  }
  // One lost connection is reported once, whichever of the link, the data path and the control channel
  // notices it first, and closing the client afterwards does not report it again.
  private lost(why: string) {
    if (this.isLost) {
      return;
    }
    this.isLost = true;
    const events = this.unacknowledgedEvents();
    this.closePath();
    this.failReady?.(new Error(why));
    this.handBack(events);
    this.emit("closed", why);
  }
  private unacknowledgedEvents(): UndeliveredEvent[] {
    return this.link
      .unacknowledged()
      .filter((message) => message.mark === EVENT)
      .map((message) => decodeSentEvent(message.msg));
  }
  private handBack(events: UndeliveredEvent[]) {
    if (events.length) {
      this.emit("undelivered", events);
    }
  }
  /** No answer can come any more: the calls waiting for one fail now, not at their timeout, and so
   *  does every call made later. */
  private controlClosed() {
    clearInterval(this.keepalive);
    this.lost("control channel closed");
    this.ended = new Error("control channel closed");
    for (const waiter of this.waiters.splice(0)) {
      waiter.fail(this.ended);
    }
  }
  private stop() {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = undefined;
    }
  }
  /** Closes the data path of a room the client is no longer in and drops what is queued for it.
   *  Left open, it would go on resending what the room has not acknowledged, to a node that no
   *  longer expects it. */
  private closePath() {
    this.stop();
    const old = this.path;
    this.path = undefined;
    old?.close();
    this.link = new Link();
    this.acknowledged?.();
  }
}

export * from "./sim.js";
export * from "./bots.js";
