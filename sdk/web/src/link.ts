import { Channel, Header, MAX_UNFRAGMENTED, decodeHeader, encodeHeader, seqNewer } from "./packet.js";

export interface Received { channel: Channel; tick: number; payload: Uint8Array; }
const WINDOW = 32, MAX_RETRIES = 10, MIN_RTO = 50, MAX_RTO = 2000;
/** Packet kind of fragment packets (their own reliable stream); channels use their own value. */
export const KIND_FRAGMENT = 3;
const FRAG_HDR = 5, FRAG_PAYLOAD = MAX_UNFRAGMENTED - FRAG_HDR, MAX_MESSAGE = 64 * 1024;

class ReliableTx {
  seq = 0; inFlight: { seq: number; payload: Uint8Array; sentAt: number; retries: number }[] = []; pending: Uint8Array[] = [];
  rtt = 100; rttVar = 50;
  rto() { return Math.min(MAX_RTO, Math.max(MIN_RTO, this.rtt + 4 * this.rttVar)); }
  onAck(ack: number, bits: number, now: number) {
    this.inFlight = this.inFlight.filter(p => {
      let acked = p.seq === ack;
      if (!acked && seqNewer(ack, p.seq)) { const d = (ack - p.seq) & 0xffff; acked = d <= 32 && ((bits >>> (d - 1)) & 1) === 1; }
      if (acked && p.retries === 0) { const s = now - p.sentAt; this.rttVar = 0.75 * this.rttVar + 0.25 * Math.abs(s - this.rtt); this.rtt = 0.875 * this.rtt + 0.125 * s; }
      return !acked;
    });
  }
}
class ReliableRx {
  ack: number | null = null; ackBits = 0; next = 0; buffer = new Map<number, Uint8Array>();
  /** Records a received seq for acks; false when already seen or too old. */
  mark(seq: number): boolean {
    if (this.ack === null) { this.ack = seq; this.ackBits = 0; return true; }
    if (seq === this.ack) return false;
    if (seqNewer(seq, this.ack)) { const sh = (seq - this.ack) & 0xffff; this.ackBits = sh >= 32 ? 0 : ((this.ackBits << sh) | (1 << (sh - 1))) >>> 0; this.ack = seq; return true; }
    const d = (this.ack - seq) & 0xffff; if (d > 32) return false;
    const bit = 1 << (d - 1); if (this.ackBits & bit) return false; this.ackBits = (this.ackBits | bit) >>> 0; return true;
  }
}
/** One reliable packet stream (ordered delivery optional). Kinds: 1 ordered, 2 unordered, 3 fragments. */
class Stream { tx = new ReliableTx(); rx = new ReliableRx(); constructor(public kind: number, public ordered: boolean) {} }

/** Mirrors net-transport::Link: unreliable-sequenced batches, reliable-ordered, reliable-unordered,
 *  and fragmentation of messages up to 64 KiB over a dedicated reliable fragment stream. */
export class Link {
  private seqTx = 0; private seqRx: number | null = null; private batch: Uint8Array[] = []; private batchLen = 0;
  private streams = new Map<number, Stream>([[1, new Stream(1, true)], [2, new Stream(2, false)], [KIND_FRAGMENT, new Stream(KIND_FRAGMENT, false)]]);
  private fragId = 0; private partial = new Map<number, { channel: Channel; parts: (Uint8Array | undefined)[]; got: number; started: number }>();
  private out: Uint8Array[] = []; tick = 0;
  stats = { sent: 0, recv: 0, retransmits: 0, dropped: 0 };

  private header(kind: number, seq: number, ackOnly = false, batch = false): Header {
    const s = this.streams.get(kind); const hasAck = !!s && s.rx.ack !== null;
    return { kind, ackOnly, hasAck, batch, seq, ack: hasAck ? s!.rx.ack! : 0, ackBits: hasAck ? s!.rx.ackBits : 0, tick: this.tick };
  }
  private push(h: Header, payload: Uint8Array) { this.out.push(encodeHeader(h, payload)); this.stats.sent++; }

  send(channel: Channel, msg: Uint8Array) {
    if (msg.length > MAX_MESSAGE) throw new Error("message too large (max 64 KiB)");
    if (msg.length > MAX_UNFRAGMENTED) {
      const count = Math.ceil(msg.length / FRAG_PAYLOAD); if (count > 255) throw new Error("message too large");
      const id = this.fragId; this.fragId = (this.fragId + 1) & 0xffff;
      for (let i = 0; i < count; i++) {
        const chunk = msg.subarray(i * FRAG_PAYLOAD, Math.min(msg.length, (i + 1) * FRAG_PAYLOAD));
        const p = new Uint8Array(FRAG_HDR + chunk.length); p[0] = id >> 8; p[1] = id & 0xff; p[2] = channel; p[3] = i; p[4] = count; p.set(chunk, FRAG_HDR);
        this.streams.get(KIND_FRAGMENT)!.tx.pending.push(p);
      }
      this.pump(performance.now()); return;
    }
    if (channel === Channel.Unreliable) {
      if (this.batchLen + 2 + msg.length > MAX_UNFRAGMENTED) this.flushBatch();
      this.batch.push(msg); this.batchLen += 2 + msg.length; return;
    }
    this.streams.get(channel)!.tx.pending.push(msg); this.pump(performance.now());
  }
  flushBatch() {
    if (this.batch.length === 0) return;
    const p = new Uint8Array(this.batchLen); let o = 0;
    for (const m of this.batch) { p[o++] = m.length >> 8; p[o++] = m.length & 0xff; p.set(m, o); o += m.length; }
    this.batch = []; this.batchLen = 0;
    const seq = this.seqTx; this.seqTx = (this.seqTx + 1) & 0xffff;
    this.push(this.header(Channel.Unreliable, seq, false, true), p);
  }
  private pump(now: number) {
    for (const s of this.streams.values()) {
      while (s.tx.pending.length && s.tx.inFlight.length < WINDOW) {
        const payload = s.tx.pending.shift()!; const seq = s.tx.seq; s.tx.seq = (s.tx.seq + 1) & 0xffff;
        s.tx.inFlight.push({ seq, payload, sentAt: now, retries: 0 });
        this.push(this.header(s.kind, seq), payload);
      }
    }
  }
  /** Timer: retransmits and flushes the batch. Returns false when the link is dead. */
  poll(now = performance.now()): boolean {
    this.flushBatch();
    for (const s of this.streams.values()) {
      const rto = s.tx.rto();
      for (const p of s.tx.inFlight) {
        if (now - p.sentAt >= rto) {
          if (p.retries >= MAX_RETRIES) return false;
          p.retries++; p.sentAt = now; this.stats.retransmits++;
          this.push(this.header(s.kind, p.seq), p.payload);
        }
      }
    }
    for (const [id, p] of this.partial) if (now - p.started > 10000) this.partial.delete(id);
    this.pump(now);
    return true;
  }
  nextOutgoing(): Uint8Array | undefined { return this.out.shift(); }
  drain(): Uint8Array[] { const o = this.out; this.out = []; return o; }

  private reassemble(frag: Uint8Array, now: number): Received | undefined {
    if (frag.length < FRAG_HDR) return;
    const id = (frag[0] << 8) | frag[1], channel = frag[2] as Channel, index = frag[3], count = frag[4];
    if (count === 0 || index >= count) return;
    let p = this.partial.get(id);
    if (!p) { if (this.partial.size >= 64) return; p = { channel, parts: new Array(count), got: 0, started: now }; this.partial.set(id, p); }
    if (p.parts.length !== count || p.channel !== channel) { this.partial.delete(id); return; }
    if (!p.parts[index]) { p.parts[index] = frag.slice(FRAG_HDR); p.got++; }
    if (p.got < count) return;
    this.partial.delete(id);
    const total = p.parts.reduce((n, x) => n + x!.length, 0); const msg = new Uint8Array(total); let o = 0;
    for (const x of p.parts) { msg.set(x!, o); o += x!.length; }
    return { channel, tick: this.tick, payload: msg };
  }

  receive(d: Uint8Array, now = performance.now()): Received[] {
    const dec = decodeHeader(d); if (!dec) return [];
    const { h, payload } = dec; this.stats.recv++;
    const out: Received[] = [];
    if (h.kind === Channel.Unreliable) {
      if (this.seqRx === null || seqNewer(h.seq, this.seqRx)) {
        this.seqRx = h.seq;
        if (h.batch) { let o = 0; while (o + 2 <= payload.length) { const n = (payload[o] << 8) | payload[o + 1]; o += 2; if (o + n > payload.length) break; out.push({ channel: Channel.Unreliable, tick: h.tick, payload: payload.subarray(o, o + n) }); o += n; } }
        else out.push({ channel: Channel.Unreliable, tick: h.tick, payload });
      } else this.stats.dropped++;
      return out;
    }
    const s = this.streams.get(h.kind); if (!s) return out;
    if (h.hasAck) { s.tx.onAck(h.ack, h.ackBits, now); this.pump(now); }
    if (!h.ackOnly) {
      if (s.rx.mark(h.seq)) {
        const deliver = (p: Uint8Array, tick: number) => {
          if (s.kind === KIND_FRAGMENT) { const r = this.reassemble(p, now); if (r) { r.tick = tick; out.push(r); } }
          else out.push({ channel: s.kind as Channel, tick, payload: p });
        };
        if (s.ordered) {
          s.rx.buffer.set(h.seq, payload.slice());
          while (s.rx.buffer.has(s.rx.next)) { deliver(s.rx.buffer.get(s.rx.next)!, h.tick); s.rx.buffer.delete(s.rx.next); s.rx.next = (s.rx.next + 1) & 0xffff; }
        } else deliver(payload.slice(), h.tick);
      }
      this.push(this.header(s.kind, s.tx.seq, true), new Uint8Array(0));
    }
    return out;
  }
}
