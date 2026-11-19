// Relay event frame: u8 code, u8 flags (bit0..1 target, bit2 cache-add, bit3 cache-remove, bit4 has-group), u16 from, [u16 group], [u16 n, n*u16 actors], payload.
export type Target = "all" | "others" | "master" | number[];
export interface EventFrame {
  code: number;
  from: number;
  target: Target;
  group?: number;
  payload: Uint8Array;
  /** Set on events the room kept for late joiners, replayed after a join. */
  cached?: boolean;
}

/** Event cache flag: `add` keeps the event for late joiners (current cache slice), `remove` drops your earlier cached events with the same code. */
export type CacheOp = "add" | "remove";

export function encodeEvent(
  code: number,
  payload: Uint8Array,
  target: Target = "others",
  group?: number,
  cache?: CacheOp,
): Uint8Array {
  const kind =
    target === "all"
      ? 0
      : target === "others"
        ? 1
        : target === "master"
          ? 2
          : 3;
  const actors = Array.isArray(target) ? target : [];
  const len =
    4 +
    (group !== undefined ? 2 : 0) +
    (kind === 3 ? 2 + actors.length * 2 : 0) +
    payload.length;
  const bytes = new Uint8Array(len);
  const dv = new DataView(bytes.buffer);
  let offset = 0;
  bytes[offset++] = code;
  bytes[offset++] =
    kind |
    (group !== undefined ? 0x10 : 0) |
    (cache === "add" ? 0x04 : cache === "remove" ? 0x08 : 0);
  dv.setUint16(offset, 0);
  offset += 2;
  if (group !== undefined) {
    dv.setUint16(offset, group);
    offset += 2;
  }
  if (kind === 3) {
    dv.setUint16(offset, actors.length);
    offset += 2;
    for (const actor of actors) {
      dv.setUint16(offset, actor);
      offset += 2;
    }
  }
  bytes.set(payload, offset);
  return bytes;
}

/** A reliable event the node did not acknowledge, with what `sendEvent` was given. */
export interface UndeliveredEvent {
  code: number;
  payload: Uint8Array;
  target: Target;
  group?: number;
  cache?: CacheOp;
}

/** Reads an event back from the bytes `encodeEvent` made of it. */
export function decodeSentEvent(bytes: Uint8Array): UndeliveredEvent {
  const { code, payload, target, group } = decodeEvent(bytes)!;
  const cache =
    bytes[1] & 0x04 ? "add" : bytes[1] & 0x08 ? "remove" : undefined;
  return { code, payload, target, group, cache };
}

export function decodeEvent(bytes: Uint8Array): EventFrame | null {
  if (bytes.length < 4) {
    return null;
  }
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let offset = 0;
  const code = bytes[offset++];
  const flags = bytes[offset++];
  const from = dv.getUint16(offset);
  offset += 2;
  let group: number | undefined;
  if (flags & 0x10) {
    group = dv.getUint16(offset);
    offset += 2;
  }
  let target: Target =
    (["all", "others", "master"] as const)[flags & 3] ?? "all";
  if ((flags & 3) === 3) {
    const count = dv.getUint16(offset);
    offset += 2;
    const actors: number[] = [];
    for (let i = 0; i < count; i++) {
      actors.push(dv.getUint16(offset));
      offset += 2;
    }
    target = actors;
  }
  return { code, from, target, group, payload: bytes.subarray(offset) };
}
