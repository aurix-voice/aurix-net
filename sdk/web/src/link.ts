import {
  Channel,
  type Header,
  MAX_UNFRAGMENTED,
  decodeHeader,
  encodeHeader,
  seqNewer,
} from "./packet.js";

export interface Received {
  channel: Channel;
  tick: number;
  payload: Uint8Array;
}
const WINDOW = 32;
/** Idle timeout of the data path, in milliseconds, and of the link on it. */
export const IDLE_TIMEOUT = 15000;
/** Bytes of reliable messages a link holds while they wait for the window. A sender that outruns
 *  its peer by more has lost it: the link ends instead of queueing without bound. */
export const RELIABLE_BACKLOG_BYTES = 1024 * 1024;
const MIN_RTO = 60;
const MAX_RTO = 1500;
/** How long a leave waits for the node to acknowledge what the game sent it reliably, in
 *  milliseconds: by then the link has sent every message again at least once, and the answer
 *  has had time to come. */
export const LEAVE_WAIT = 2 * MAX_RTO;
/** Packet kind of fragment packets (their own reliable stream); channels use their own value. */
export const KIND_FRAGMENT = 3;
const FRAG_HDR = 5;
const FRAG_PAYLOAD = MAX_UNFRAGMENTED - FRAG_HDR;
const MAX_MESSAGE = 64 * 1024;

/** The messages of one unreliable channel that leave in the next datagram. */
class Batch {
  seq = 0;
  private messages: Uint8Array[] = [];
  private length = 0;

  fits(message: Uint8Array): boolean {
    return this.length + 2 + message.length <= MAX_UNFRAGMENTED;
  }
  add(message: Uint8Array) {
    this.messages.push(message);
    this.length += 2 + message.length;
  }
  take(): Uint8Array | undefined {
    if (this.messages.length === 0) {
      return undefined;
    }
    const packet = new Uint8Array(this.length);
    let offset = 0;
    for (const message of this.messages) {
      packet[offset++] = message.length >> 8;
      packet[offset++] = message.length & 0xff;
      packet.set(message, offset);
      offset += message.length;
    }
    this.messages = [];
    this.length = 0;
    return packet;
  }
}

/** A reliable message the peer has not acknowledged, with the mark its sender gave it. */
export interface Unacknowledged {
  channel: Channel;
  msg: Uint8Array;
  mark: number;
}

/** A reliable message and where its packets are among those queued on a stream. */
interface Queued extends Unacknowledged {
  order: number;
  first: number;
  count: number;
}

class ReliableTx {
  seq = 0;
  /** Packets ever queued: the nth of them gets seq n mod 65536. */
  queued = 0;
  /** The reliable messages queued, from the oldest one not acknowledged yet. */
  log: Queued[] = [];
  inFlight: {
    seq: number;
    payload: Uint8Array;
    sentAt: number;
    firstSent: number;
    resent: boolean;
  }[] = [];
  pending: Uint8Array[] = [];
  pendingBytes = 0;
  /** Drops what `ack` and `bits` acknowledge; returns the round trips of the packets sent once. */
  onAck(ack: number, bits: number, now: number): number[] {
    const samples: number[] = [];
    this.inFlight = this.inFlight.filter((packet) => {
      let acked = packet.seq === ack;
      if (!acked && seqNewer(ack, packet.seq)) {
        const distance = (ack - packet.seq) & 0xffff;
        acked = distance <= 32 && ((bits >>> (distance - 1)) & 1) === 1;
      }
      if (acked && !packet.resent) {
        samples.push(now - packet.sentAt);
      }
      return !acked;
    });
    while (this.log.length && !this.holds(this.log[0])) {
      this.log.shift();
    }
    return samples;
  }
  /** Whether a packet of `message` waits for the window or for its acknowledgement. */
  holds(message: Queued): boolean {
    const sent = this.queued - this.pending.length;
    for (let n = message.first; n < message.first + message.count; n++) {
      if (n >= sent || this.inFlight.some((p) => p.seq === (n & 0xffff))) {
        return true;
      }
    }
    return false;
  }
}
class ReliableRx {
  ack: number | null = null;
  ackBits = 0;
  next = 0;
  buffer = new Map<number, Uint8Array>();
  /** Records a received seq for acks; false when already seen or too old. */
  mark(seq: number): boolean {
    if (this.ack === null) {
      this.ack = seq;
      this.ackBits = 0;
      return true;
    }
    if (seq === this.ack) {
      return false;
    }
    if (seqNewer(seq, this.ack)) {
      const sh = (seq - this.ack) & 0xffff;
      this.ackBits =
        sh >= 32 ? 0 : ((this.ackBits << sh) | (1 << (sh - 1))) >>> 0;
      this.ack = seq;
      return true;
    }
    const distance = (this.ack - seq) & 0xffff;
    if (distance > 32) {
      return false;
    }
    const bit = 1 << (distance - 1);
    if (this.ackBits & bit) {
      return false;
    }
    this.ackBits = (this.ackBits | bit) >>> 0;
    return true;
  }
}
/** One reliable packet stream (ordered delivery optional). Kinds: 1 ordered, 2 unordered, 3 fragments. */
class Stream {
  tx = new ReliableTx();
  rx = new ReliableRx();
  constructor(
    public kind: number,
    public ordered: boolean,
  ) {}
}

/** Mirrors net-transport::Link: unreliable batches (sequenced and unsequenced), reliable-ordered,
 *  reliable-unordered, and fragmentation of messages up to 64 KiB over a dedicated reliable fragment stream. */
export class Link {
  private seqRx: number | null = null;
  private batches = new Map<number, Batch>([
    [Channel.Unreliable, new Batch()],
    [Channel.UnreliableUnsequenced, new Batch()],
  ]);
  private streams = new Map<number, Stream>([
    [1, new Stream(1, true)],
    [2, new Stream(2, false)],
    [KIND_FRAGMENT, new Stream(KIND_FRAGMENT, false)],
  ]);
  private fragId = 0;
  private partial = new Map<
    number,
    {
      channel: Channel;
      parts: (Uint8Array | undefined)[];
      got: number;
      started: number;
    }
  >();
  private out: Uint8Array[] = [];
  private srtt: number | null = null;
  private rttVar = 0;
  private dead = false;
  private order = 0;
  private lost: Unacknowledged[] = [];
  tick = 0;
  stats = { sent: 0, recv: 0, retransmits: 0, dropped: 0 };

  /** `idle` is the idle timeout of what carries the datagrams: a link whose oldest reliable
   *  packet has gone that long without an acknowledgement is dead. */
  constructor(private idle = IDLE_TIMEOUT) {}

  private rto() {
    return this.srtt === null
      ? 250
      : Math.min(MAX_RTO, Math.max(MIN_RTO, this.srtt + 4 * this.rttVar));
  }

  get isDead() {
    return this.dead;
  }
  /** Bytes of reliable messages waiting for the window. */
  backlog() {
    let bytes = 0;
    for (const stream of this.streams.values()) {
      bytes += stream.tx.pendingBytes;
    }
    return bytes;
  }
  /** The reliable messages the peer has not acknowledged, in the order they were queued; on a
   *  link that has ended, those it had not acknowledged by then. The peer may have taken any of
   *  them: an acknowledgement can be lost too. */
  unacknowledged(): Unacknowledged[] {
    if (this.dead) {
      return this.lost;
    }
    const waiting: Queued[] = [];
    for (const stream of this.streams.values()) {
      waiting.push(...stream.tx.log.filter((m) => stream.tx.holds(m)));
    }
    return waiting
      .sort((a, b) => a.order - b.order)
      .map(({ channel, msg, mark }) => ({ channel, msg, mark }));
  }
  /** Ends the link: nothing more is sent or queued, and what was queued is dropped. */
  close() {
    this.lost = this.unacknowledged();
    this.dead = true;
    for (const stream of this.streams.values()) {
      stream.tx.inFlight = [];
      stream.tx.pending = [];
      stream.tx.pendingBytes = 0;
      stream.tx.log = [];
    }
    for (const batch of this.batches.values()) {
      batch.take();
    }
    this.out = [];
  }

  private header(
    kind: number,
    seq: number,
    ackOnly = false,
    batch = false,
  ): Header {
    const stream = this.streams.get(kind);
    const hasAck = !!stream && stream.rx.ack !== null;
    return {
      kind,
      ackOnly,
      hasAck,
      batch,
      seq,
      ack: hasAck ? stream.rx.ack! : 0,
      ackBits: hasAck ? stream.rx.ackBits : 0,
      tick: this.tick,
    };
  }
  private push(header: Header, payload: Uint8Array) {
    this.out.push(encodeHeader(header, payload));
    this.stats.sent++;
  }

  /** Queues a message. Throws on a dead link, and ends the link when the message would not fit
   *  in `RELIABLE_BACKLOG_BYTES`. `mark` comes back with a reliable message in `unacknowledged()`. */
  send(channel: Channel, msg: Uint8Array, now = performance.now(), mark = 0) {
    if (this.dead) {
      throw new Error("peer stopped acknowledging; link dead");
    }
    if (msg.length > MAX_MESSAGE) {
      throw new Error("message too large (max 64 KiB)");
    }
    if (typeof Channel[channel] !== "string") {
      throw new RangeError(`unknown channel ${channel}`);
    }
    const reliable =
      channel === Channel.Reliable || channel === Channel.ReliableUnordered;
    const queued = msg.length > MAX_UNFRAGMENTED || reliable;
    if (queued && this.backlog() + msg.length > RELIABLE_BACKLOG_BYTES) {
      this.close();
      throw new Error("reliable messages outran the peer; link dead");
    }
    if (msg.length > MAX_UNFRAGMENTED) {
      const count = Math.ceil(msg.length / FRAG_PAYLOAD);
      if (count > 255) {
        throw new Error("message too large");
      }
      const id = this.fragId;
      this.fragId = (this.fragId + 1) & 0xffff;
      const stream = this.streams.get(KIND_FRAGMENT)!;
      if (reliable) {
        this.note(stream, channel, msg, mark, count);
      }
      for (let i = 0; i < count; i++) {
        const chunk = msg.subarray(
          i * FRAG_PAYLOAD,
          Math.min(msg.length, (i + 1) * FRAG_PAYLOAD),
        );
        const packet = new Uint8Array(FRAG_HDR + chunk.length);
        packet[0] = id >> 8;
        packet[1] = id & 0xff;
        packet[2] = channel;
        packet[3] = i;
        packet[4] = count;
        packet.set(chunk, FRAG_HDR);
        this.queue(stream, packet);
      }
      this.pump(now);
      return;
    }
    const batch = this.batches.get(channel);
    if (batch) {
      if (!batch.fits(msg)) {
        this.flushBatch();
      }
      batch.add(msg);
      return;
    }
    const stream = this.streams.get(channel)!;
    this.note(stream, channel, msg, mark, 1);
    this.queue(stream, msg);
    this.pump(now);
  }
  private note(
    stream: Stream,
    channel: Channel,
    msg: Uint8Array,
    mark: number,
    count: number,
  ) {
    stream.tx.log.push({
      order: this.order++,
      channel,
      msg,
      mark,
      first: stream.tx.queued,
      count,
    });
  }
  private queue(stream: Stream, packet: Uint8Array) {
    stream.tx.queued++;
    stream.tx.pending.push(packet);
    stream.tx.pendingBytes += packet.length;
  }
  flushBatch() {
    for (const [channel, batch] of this.batches) {
      const packet = batch.take();
      if (packet) {
        const seq = batch.seq;
        batch.seq = (batch.seq + 1) & 0xffff;
        this.push(this.header(channel, seq, false, true), packet);
      }
    }
  }
  private pump(now: number) {
    for (const stream of this.streams.values()) {
      // bound the span of unacked seqs, not their count: the peer can only ack 32 seqs behind the newest it has seen
      while (
        stream.tx.pending.length &&
        ((stream.tx.seq - (stream.tx.inFlight[0]?.seq ?? stream.tx.seq)) &
          0xffff) <
          WINDOW
      ) {
        const payload = stream.tx.pending.shift()!;
        stream.tx.pendingBytes -= payload.length;
        const seq = stream.tx.seq;
        stream.tx.seq = (stream.tx.seq + 1) & 0xffff;
        stream.tx.inFlight.push({
          seq,
          payload,
          sentAt: now,
          firstSent: now,
          resent: false,
        });
        this.push(this.header(stream.kind, seq), payload);
      }
    }
  }
  /** Timer: retransmits and flushes the batch. Returns false when the link is dead. */
  poll(now = performance.now()): boolean {
    if (this.dead) {
      return false;
    }
    this.flushBatch();
    // The link is lost when a packet that was resent has waited the idle time for its
    // acknowledgement. One whose time ran out before any resend, because this timer came late,
    // is resent below and waits its time again.
    let lost = false;
    for (const stream of this.streams.values()) {
      for (const packet of stream.tx.inFlight) {
        if (now - packet.firstSent < this.idle) {
          continue;
        }
        if (packet.resent) {
          lost = true;
        } else {
          packet.firstSent = now;
        }
      }
    }
    if (lost) {
      this.close();
      return false;
    }
    const rto = this.rto();
    for (const stream of this.streams.values()) {
      for (const packet of stream.tx.inFlight) {
        if (now - packet.sentAt >= rto) {
          packet.resent = true;
          packet.sentAt = now;
          this.stats.retransmits++;
          this.push(this.header(stream.kind, packet.seq), packet.payload);
        }
      }
    }
    for (const [id, packet] of this.partial) {
      if (now - packet.started > 10000) {
        this.partial.delete(id);
      }
    }
    this.pump(now);
    return true;
  }
  nextOutgoing(): Uint8Array | undefined {
    return this.out.shift();
  }
  drain(): Uint8Array[] {
    const packets = this.out;
    this.out = [];
    return packets;
  }

  private reassemble(frag: Uint8Array, now: number): Received | undefined {
    if (frag.length < FRAG_HDR) {
      return;
    }
    const id = (frag[0] << 8) | frag[1];
    const channel: Channel = frag[2];
    const index = frag[3];
    const count = frag[4];
    if (count === 0 || index >= count) {
      return;
    }
    let packet = this.partial.get(id);
    if (!packet) {
      if (this.partial.size >= 64) {
        return;
      }
      packet = {
        channel,
        parts: new Array<Uint8Array | undefined>(count),
        got: 0,
        started: now,
      };
      this.partial.set(id, packet);
    }
    if (packet.parts.length !== count || packet.channel !== channel) {
      this.partial.delete(id);
      return;
    }
    if (!packet.parts[index]) {
      packet.parts[index] = frag.slice(FRAG_HDR);
      packet.got++;
    }
    if (packet.got < count) {
      return;
    }
    this.partial.delete(id);
    const total = packet.parts.reduce(
      (length, part) => length + part!.length,
      0,
    );
    const msg = new Uint8Array(total);
    let offset = 0;
    for (const part of packet.parts) {
      msg.set(part!, offset);
      offset += part!.length;
    }
    return { channel, tick: this.tick, payload: msg };
  }

  receive(datagram: Uint8Array, now = performance.now()): Received[] {
    const dec = decodeHeader(datagram);
    if (!dec) {
      return [];
    }
    const { h: header, payload } = dec;
    this.stats.recv++;
    const out: Received[] = [];
    if (this.batches.has(header.kind)) {
      const channel: Channel = header.kind;
      const sequenced = channel === Channel.Unreliable;
      if (
        !sequenced ||
        this.seqRx === null ||
        seqNewer(header.seq, this.seqRx)
      ) {
        if (sequenced) {
          this.seqRx = header.seq;
        }
        if (header.batch) {
          let offset = 0;
          while (offset + 2 <= payload.length) {
            const length = (payload[offset] << 8) | payload[offset + 1];
            offset += 2;
            if (offset + length > payload.length) {
              break;
            }
            out.push({
              channel,
              tick: header.tick,
              payload: payload.subarray(offset, offset + length),
            });
            offset += length;
          }
        } else {
          out.push({ channel, tick: header.tick, payload });
        }
      } else {
        this.stats.dropped++;
      }
      return out;
    }
    const stream = this.streams.get(header.kind);
    if (!stream) {
      return out;
    }
    if (header.hasAck) {
      for (const sample of stream.tx.onAck(header.ack, header.ackBits, now)) {
        if (this.srtt === null) {
          this.srtt = sample;
          this.rttVar = sample / 2;
        } else {
          this.rttVar =
            0.75 * this.rttVar + 0.25 * Math.abs(this.srtt - sample);
          this.srtt = 0.875 * this.srtt + 0.125 * sample;
        }
      }
      this.pump(now);
    }
    if (!header.ackOnly) {
      if (stream.rx.mark(header.seq)) {
        const deliver = (packet: Uint8Array, tick: number) => {
          if (stream.kind === KIND_FRAGMENT) {
            const message = this.reassemble(packet, now);
            if (message) {
              message.tick = tick;
              out.push(message);
            }
          } else {
            out.push({ channel: stream.kind, tick, payload: packet });
          }
        };
        if (stream.ordered) {
          stream.rx.buffer.set(header.seq, payload.slice());
          while (stream.rx.buffer.has(stream.rx.next)) {
            deliver(stream.rx.buffer.get(stream.rx.next)!, header.tick);
            stream.rx.buffer.delete(stream.rx.next);
            stream.rx.next = (stream.rx.next + 1) & 0xffff;
          }
        } else {
          deliver(payload.slice(), header.tick);
        }
      }
      this.push(
        this.header(stream.kind, stream.tx.seq, true),
        new Uint8Array(0),
      );
    }
    return out;
  }
}
