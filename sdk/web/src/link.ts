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
const MAX_RETRIES = 10;
const MIN_RTO = 50;
const MAX_RTO = 2000;
/** Packet kind of fragment packets (their own reliable stream); channels use their own value. */
export const KIND_FRAGMENT = 3;
const KIND_UNRELIABLE: number = Channel.Unreliable;
const FRAG_HDR = 5;
const FRAG_PAYLOAD = MAX_UNFRAGMENTED - FRAG_HDR;
const MAX_MESSAGE = 64 * 1024;

class ReliableTx {
  seq = 0;
  inFlight: {
    seq: number;
    payload: Uint8Array;
    sentAt: number;
    retries: number;
  }[] = [];
  pending: Uint8Array[] = [];
  rtt = 100;
  rttVar = 50;
  rto() {
    return Math.min(MAX_RTO, Math.max(MIN_RTO, this.rtt + 4 * this.rttVar));
  }
  onAck(ack: number, bits: number, now: number) {
    this.inFlight = this.inFlight.filter((packet) => {
      let acked = packet.seq === ack;
      if (!acked && seqNewer(ack, packet.seq)) {
        const distance = (ack - packet.seq) & 0xffff;
        acked = distance <= 32 && ((bits >>> (distance - 1)) & 1) === 1;
      }
      if (acked && packet.retries === 0) {
        const elapsed = now - packet.sentAt;
        this.rttVar = 0.75 * this.rttVar + 0.25 * Math.abs(elapsed - this.rtt);
        this.rtt = 0.875 * this.rtt + 0.125 * elapsed;
      }
      return !acked;
    });
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

/** Mirrors net-transport::Link: unreliable-sequenced batches, reliable-ordered, reliable-unordered,
 *  and fragmentation of messages up to 64 KiB over a dedicated reliable fragment stream. */
export class Link {
  private seqTx = 0;
  private seqRx: number | null = null;
  private batch: Uint8Array[] = [];
  private batchLen = 0;
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
  tick = 0;
  stats = { sent: 0, recv: 0, retransmits: 0, dropped: 0 };

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

  send(channel: Channel, msg: Uint8Array) {
    if (msg.length > MAX_MESSAGE) {
      throw new Error("message too large (max 64 KiB)");
    }
    if (msg.length > MAX_UNFRAGMENTED) {
      const count = Math.ceil(msg.length / FRAG_PAYLOAD);
      if (count > 255) {
        throw new Error("message too large");
      }
      const id = this.fragId;
      this.fragId = (this.fragId + 1) & 0xffff;
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
        this.streams.get(KIND_FRAGMENT)!.tx.pending.push(packet);
      }
      this.pump(performance.now());
      return;
    }
    if (channel === Channel.Unreliable) {
      if (this.batchLen + 2 + msg.length > MAX_UNFRAGMENTED) {
        this.flushBatch();
      }
      this.batch.push(msg);
      this.batchLen += 2 + msg.length;
      return;
    }
    this.streams.get(channel)!.tx.pending.push(msg);
    this.pump(performance.now());
  }
  flushBatch() {
    if (this.batch.length === 0) {
      return;
    }
    const packet = new Uint8Array(this.batchLen);
    let offset = 0;
    for (const message of this.batch) {
      packet[offset++] = message.length >> 8;
      packet[offset++] = message.length & 0xff;
      packet.set(message, offset);
      offset += message.length;
    }
    this.batch = [];
    this.batchLen = 0;
    const seq = this.seqTx;
    this.seqTx = (this.seqTx + 1) & 0xffff;
    this.push(this.header(Channel.Unreliable, seq, false, true), packet);
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
        const seq = stream.tx.seq;
        stream.tx.seq = (stream.tx.seq + 1) & 0xffff;
        stream.tx.inFlight.push({ seq, payload, sentAt: now, retries: 0 });
        this.push(this.header(stream.kind, seq), payload);
      }
    }
  }
  /** Timer: retransmits and flushes the batch. Returns false when the link is dead. */
  poll(now = performance.now()): boolean {
    this.flushBatch();
    for (const stream of this.streams.values()) {
      const rto = stream.tx.rto();
      for (const packet of stream.tx.inFlight) {
        if (now - packet.sentAt >= rto) {
          if (packet.retries >= MAX_RETRIES) {
            return false;
          }
          packet.retries++;
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
    if (header.kind === KIND_UNRELIABLE) {
      if (this.seqRx === null || seqNewer(header.seq, this.seqRx)) {
        this.seqRx = header.seq;
        if (header.batch) {
          let offset = 0;
          while (offset + 2 <= payload.length) {
            const length = (payload[offset] << 8) | payload[offset + 1];
            offset += 2;
            if (offset + length > payload.length) {
              break;
            }
            out.push({
              channel: Channel.Unreliable,
              tick: header.tick,
              payload: payload.subarray(offset, offset + length),
            });
            offset += length;
          }
        } else {
          out.push({ channel: Channel.Unreliable, tick: header.tick, payload });
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
      stream.tx.onAck(header.ack, header.ackBits, now);
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
