// Relay event frame: u8 code, u8 flags (bit0..1 target, bit2 cache-add, bit3 cache-remove, bit4 has-group), u16 from, [u16 group], [u16 n, n*u16 actors], payload.
export type Target = "all" | "others" | "master" | number[];
export interface EventFrame { code: number; from: number; target: Target; group?: number; payload: Uint8Array; }

export function encodeEvent(code: number, payload: Uint8Array, target: Target = "others", group?: number): Uint8Array {
  const kind = target === "all" ? 0 : target === "others" ? 1 : target === "master" ? 2 : 3;
  const actors = Array.isArray(target) ? target : [];
  const len = 4 + (group !== undefined ? 2 : 0) + (kind === 3 ? 2 + actors.length * 2 : 0) + payload.length;
  const b = new Uint8Array(len); const dv = new DataView(b.buffer); let o = 0;
  b[o++] = code; b[o++] = kind | (group !== undefined ? 0x10 : 0); dv.setUint16(o, 0); o += 2;
  if (group !== undefined) { dv.setUint16(o, group); o += 2; }
  if (kind === 3) { dv.setUint16(o, actors.length); o += 2; for (const a of actors) { dv.setUint16(o, a); o += 2; } }
  b.set(payload, o);
  return b;
}

export function decodeEvent(b: Uint8Array): EventFrame | null {
  if (b.length < 4) return null;
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength); let o = 0;
  const code = b[o++], flags = b[o++]; const from = dv.getUint16(o); o += 2;
  let group: number | undefined; if (flags & 0x10) { group = dv.getUint16(o); o += 2; }
  let target: Target = (["all", "others", "master"] as const)[flags & 3] ?? "all";
  if ((flags & 3) === 3) { const n = dv.getUint16(o); o += 2; const a: number[] = []; for (let i = 0; i < n; i++) { a.push(dv.getUint16(o)); o += 2; } target = a; }
  return { code, from, target, group, payload: b.subarray(o) };
}
