// 12-byte datagram header, big-endian: u8 version, u8 kind, u16 seq, u16 ack, u32 ack_bits, u16 tick.
export const PROTOCOL_VERSION = 1;
export const HEADER_LEN = 12;
export const MAX_DATAGRAM = 1200;
export const MAX_UNFRAGMENTED = MAX_DATAGRAM - HEADER_LEN;
export const FLAG_ACK_ONLY = 0x80, FLAG_HAS_ACK = 0x40, FLAG_BATCH = 0x20;
export enum Channel { Unreliable = 0, Reliable = 1, ReliableUnordered = 2 }
export const KIND_FRAGMENT = 3;

export interface Header { kind: number; ackOnly: boolean; hasAck: boolean; batch: boolean; seq: number; ack: number; ackBits: number; tick: number; }

export function encodeHeader(h: Header, payload: Uint8Array): Uint8Array {
  const out = new Uint8Array(HEADER_LEN + payload.length);
  const dv = new DataView(out.buffer);
  out[0] = PROTOCOL_VERSION;
  out[1] = (h.kind & 0x1f) | (h.ackOnly ? FLAG_ACK_ONLY : 0) | (h.hasAck ? FLAG_HAS_ACK : 0) | (h.batch ? FLAG_BATCH : 0);
  dv.setUint16(2, h.seq); dv.setUint16(4, h.ack); dv.setUint32(6, h.ackBits); dv.setUint16(10, h.tick);
  out.set(payload, HEADER_LEN);
  return out;
}

export function decodeHeader(d: Uint8Array): { h: Header; payload: Uint8Array } | null {
  if (d.length < HEADER_LEN || d[0] !== PROTOCOL_VERSION) return null;
  const dv = new DataView(d.buffer, d.byteOffset, d.byteLength);
  const k = d[1];
  return {
    h: { kind: k & 0x1f, ackOnly: !!(k & FLAG_ACK_ONLY), hasAck: !!(k & FLAG_HAS_ACK), batch: !!(k & FLAG_BATCH), seq: dv.getUint16(2), ack: dv.getUint16(4), ackBits: dv.getUint32(6), tick: dv.getUint16(10) },
    payload: d.subarray(HEADER_LEN),
  };
}

/** true when a is a newer 16-bit sequence than b */
export function seqNewer(a: number, b: number): boolean { return ((a - b) & 0xffff) < 0x8000 && a !== b; }
