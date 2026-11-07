import { Channel, Header, MAX_UNFRAGMENTED, decodeHeader, encodeHeader, seqNewer } from "./packet.js";

export interface Received { channel: Channel; tick: number; payload: Uint8Array; }
const WINDOW = 32, MAX_RETRIES = 10, MIN_RTO = 50, MAX_RTO = 2000;

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
  mark(seq: number): boolean {
    if (this.ack === null) { this.ack = seq; this.ackBits = 0; return true; }
    if (seq === this.ack) return false;
    if (seqNewer(seq, this.ack)) { const sh = (seq - this.ack) & 0xffff; this.ackBits = sh >= 32 ? 0 : ((this.ackBits << sh) | (1 << (sh - 1))) >>> 0; this.ack = seq; return true; }
    const d = (this.ack - seq) & 0xffff; if (d > 32) return false;
    const bit = 1 << (d - 1); if (this.ackBits & bit) return false; this.ackBits = (this.ackBits | bit) >>> 0; return true;
  }
}

/** Mirrors net-transport::Link for the WebSocket tunnel (unreliable-sequenced + reliable-ordered). */
export class Link {
  private seqTx = 0; private seqRx: number | null = null; private batch: Uint8Array[] = []; private batchLen = 0;
  private tx = new ReliableTx(); private rx = new ReliableRx(); private out: Uint8Array[] = []; tick = 0;
  stats = { sent: 0, recv: 0, retransmits: 0, dropped: 0 };

  private header(kind: number, seq: number, ackOnly = false, batch = false): Header {
    const hasAck = kind === Channel.Reliable && this.rx.ack !== null;
    return { kind, ackOnly, hasAck, batch, seq, ack: hasAck ? this.rx.ack! : 0, ackBits: hasAck ? this.rx.ackBits : 0, tick: this.tick };
  }
  private push(h: Header, payload: Uint8Array) { this.out.push(encodeHeader(h, payload)); this.stats.sent++; }

  send(channel: Channel, msg: Uint8Array) {
    if (msg.length > MAX_UNFRAGMENTED) throw new Error("message too large (fragmentation not implemented in web sdk)");
    if (channel === Channel.Unreliable) {
      if (this.batchLen + 2 + msg.length > MAX_UNFRAGMENTED) this.flushBatch();
      this.batch.push(msg); this.batchLen += 2 + msg.length; return;
    }
    if (channel !== Channel.Reliable) throw new Error("reliable-unordered not implemented in web sdk");
    this.tx.pending.push(msg); this.pump(performance.now());
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
    while (this.tx.pending.length && this.tx.inFlight.length < WINDOW) {
      const payload = this.tx.pending.shift()!; const seq = this.tx.seq; this.tx.seq = (this.tx.seq + 1) & 0xffff;
      this.tx.inFlight.push({ seq, payload, sentAt: now, retries: 0 });
      this.push(this.header(Channel.Reliable, seq), payload);
    }
  }
  /** Timer: retransmits and flushes the batch. Returns false when the link is dead. */
  poll(now = performance.now()): boolean {
    this.flushBatch();
    const rto = this.tx.rto();
    for (const p of this.tx.inFlight) {
      if (now - p.sentAt >= rto) {
        if (p.retries >= MAX_RETRIES) return false;
        p.retries++; p.sentAt = now; this.stats.retransmits++;
        this.push(this.header(Channel.Reliable, p.seq), p.payload);
      }
    }
    this.pump(now);
    return true;
  }
  nextOutgoing(): Uint8Array | undefined { return this.out.shift(); }
  drain(): Uint8Array[] { const o = this.out; this.out = []; return o; }

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
    } else if (h.kind === Channel.Reliable) {
      if (h.hasAck) { this.tx.onAck(h.ack, h.ackBits, now); this.pump(now); }
      if (!h.ackOnly) {
        if (this.rx.mark(h.seq)) {
          this.rx.buffer.set(h.seq, payload.slice());
          while (this.rx.buffer.has(this.rx.next)) { out.push({ channel: Channel.Reliable, tick: h.tick, payload: this.rx.buffer.get(this.rx.next)! }); this.rx.buffer.delete(this.rx.next); this.rx.next = (this.rx.next + 1) & 0xffff; }
        }
        // ack back (piggybacked on an empty ack-only packet)
        this.push(this.header(Channel.Reliable, this.tx.seq, true), new Uint8Array(0));
      }
    }
    return out;
  }
}
