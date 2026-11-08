// Sync rooms for the browser: schema IDL, MSB-first bit codec, delta decoding, acks, owner inputs and
// an interpolation buffer — a port of the managed C# core (mirrors net-wire::{bits,schema,value}).

export type SyncOp =
  | { op: "write"; entity: number; writes: Array<[ComponentDef, ComponentValue]> }
  | { op: "spawn"; writes: Array<[ComponentDef, ComponentValue]> }
  | { op: "despawn"; entity: number }
  | { op: "transfer"; entity: number; to: number };

export const KIND_INPUT = 0x10, KIND_DELTA = 0x11, KIND_RESYNC = 0x12, KIND_ACK = 0x13;

export class SyncError extends Error {}

// ---------------- bits ----------------
export class BitWriter {
  private buf: number[] = []; private scratch = 0n; private n = 0; bitLen = 0;
  writeBits(v: bigint | number, nbits: number): void {
    if (nbits === 0) return;
    let x = BigInt.asUintN(64, BigInt(v));
    if (nbits < 64) x &= (1n << BigInt(nbits)) - 1n;
    this.bitLen += nbits;
    let left = nbits;
    while (left > 0) {
      const take = Math.min(left, 64 - this.n);
      const part = (x >> BigInt(left - take)) & ((1n << BigInt(take)) - 1n);
      this.scratch = take === 64 ? part : ((this.scratch << BigInt(take)) | part) & ((1n << 64n) - 1n);
      this.n += take; left -= take;
      while (this.n >= 8) { this.buf.push(Number((this.scratch >> BigInt(this.n - 8)) & 0xffn)); this.n -= 8; }
    }
  }
  writeBool(b: boolean): void { this.writeBits(b ? 1 : 0, 1); }
  writeUInt(v: bigint | number, nbits: number): void { const max = nbits >= 64 ? (1n << 64n) - 1n : (1n << BigInt(nbits)) - 1n; const x = BigInt(v); this.writeBits(x > max ? max : x, nbits); }
  writeInt(v: bigint | number, nbits: number): void { const x = BigInt.asIntN(64, BigInt(v)); this.writeUInt(BigInt.asUintN(64, (x << 1n) ^ (x >> 63n)), nbits); }
  writeQuantized(v: number, min: number, max: number, res: number): void { const { bits } = quantBits(min, max, res); const q = Math.floor((Math.min(Math.max(v, min), max) - min) / res + 0.5); this.writeUInt(q, bits); }
  writeF32(v: number): void { const dv = new DataView(new ArrayBuffer(4)); dv.setFloat32(0, v); this.writeBits(dv.getUint32(0), 32); }
  writeBytes(b: Uint8Array): void { for (const x of b) this.writeBits(x, 8); }
  finish(): Uint8Array { const o = this.buf.slice(); if (this.n > 0) o.push(Number((this.scratch << BigInt(8 - this.n)) & 0xffn)); return Uint8Array.from(o); }
}

export class BitReader {
  private pos = 0; private readonly bits: number;
  constructor(private readonly b: Uint8Array, private readonly off = 0) { this.bits = (b.length - off) * 8; }
  get bitsLeft(): number { return this.bits - this.pos; }
  readBig(nbits: number): bigint {
    if (nbits > this.bitsLeft) throw new SyncError("delta truncated");
    let v = 0n;
    for (let i = 0; i < nbits; i++) { const p = this.pos + i; v = (v << 1n) | BigInt((this.b[this.off + (p >> 3)] >> (7 - (p & 7))) & 1); }
    this.pos += nbits; return v;
  }
  /** Up to 53 bits as a number. */
  readBits(nbits: number): number { if (nbits > 53) throw new SyncError("readBits > 53"); return Number(this.readBig(nbits)); }
  readBool(): boolean { return this.readBits(1) === 1; }
  readUInt(nbits: number): bigint { return this.readBig(nbits); }
  readInt(nbits: number): bigint { const zz = this.readBig(nbits); return (zz >> 1n) ^ -(zz & 1n); }
  readQuantized(min: number, max: number, res: number): number { const { bits } = quantBits(min, max, res); return min + Number(this.readBig(bits)) * res; }
  readF32(): number { const dv = new DataView(new ArrayBuffer(4)); dv.setUint32(0, this.readBits(32)); return dv.getFloat32(0); }
  readBytes(n: number): Uint8Array { const o = new Uint8Array(n); for (let i = 0; i < n; i++) o[i] = this.readBits(8); return o; }
}

export function quantBits(min: number, max: number, res: number): { bits: number; steps: number } {
  const steps = Math.max(1, Math.ceil((max - min) / res));
  return { bits: Math.max(1, bitLength(steps)), steps };
}
function bitLength(v: number): number { let n = 0; let x = Math.floor(v); while (x > 0) { n++; x = Math.floor(x / 2); } return n; }
export function lenBits(max: number): number { return Math.max(1, bitLength(max)); }

// ---------------- schema ----------------
export type FieldKind = "bool" | "uint" | "int" | "f32" | "quant" | "vec2" | "vec3" | "quat" | "string" | "bytes";
export interface FieldType { kind: FieldKind; bits: number; min: number; max: number; res: number; maxLen: number }
export interface Field { name: string; type: FieldType }
export interface ComponentDef { id: number; name: string; ownerOnly: boolean; global: boolean; fields: Field[] }

export class Schema {
  components: ComponentDef[] = [];
  byId(id: number): ComponentDef | undefined { return this.components[id]; }
  byName(n: string): ComponentDef | undefined { return this.components.find((c) => c.name === n); }
  get componentIdBits(): number { return Math.max(1, bitLength(Math.max(2, this.components.length) - 1)); }
  indexOf(c: ComponentDef, field: string): number { return c.fields.findIndex((f) => f.name === field); }

  static parse(text: string): Schema {
    const s = new Schema(); let cur: ComponentDef | null = null; let line = 0;
    for (const raw of text.split("\n")) {
      line++;
      let l = raw; const c = l.indexOf("//"); if (c >= 0) l = l.slice(0, c); l = l.trim();
      if (!l) continue;
      if (l.startsWith("component ")) {
        if (cur) throw new SyncError(`schema line ${line}: nested component`);
        const rest = l.slice(10).trim();
        if (!rest.endsWith("{")) throw new SyncError(`schema line ${line}: expected '{'`);
        const parts = rest.slice(0, -1).trim().split(/[ \t]+/).filter(Boolean);
        cur = { id: s.components.length, name: parts[0], ownerOnly: false, global: false, fields: [] };
        for (const p of parts.slice(1)) { if (p === "@owner_only") cur.ownerOnly = true; else if (p === "@global") cur.global = true; else throw new SyncError(`schema line ${line}: unknown flag ${p}`); }
        continue;
      }
      if (l === "}") { if (!cur) throw new SyncError(`schema line ${line}: stray '}'`); s.components.push(cur); cur = null; continue; }
      if (!cur) throw new SyncError(`schema line ${line}: field outside component`);
      const colon = l.indexOf(":"); if (colon < 0) throw new SyncError(`schema line ${line}: expected name: type`);
      const name = l.slice(0, colon).trim(); let ty = l.slice(colon + 1).trim();
      let attr: string | null = null; const at = ty.indexOf("@"); if (at >= 0) { attr = ty.slice(at + 1).trim(); ty = ty.slice(0, at).trim(); }
      cur.fields.push({ name, type: parseType(ty, attr, line) });
    }
    if (cur) throw new SyncError("schema: unterminated component");
    return s;
  }
}

function ft(kind: FieldKind, p: Partial<FieldType> = {}): FieldType { return { kind, bits: 0, min: 0, max: 0, res: 0, maxLen: 0, ...p }; }
function parseType(b: string, attr: string | null, line: number): FieldType {
  const range = (k: FieldKind): FieldType => {
    if (!attr || !attr.startsWith("range(") || !attr.endsWith(")")) throw new SyncError(`schema line ${line}: ${b} needs @range(min, max, res)`);
    const p = attr.slice(6, -1).split(",");
    if (p.length !== 3) throw new SyncError(`schema line ${line}: bad range`);
    const [min, max, res] = p.map((x) => { const f = parseFloat(x.trim()); if (Number.isNaN(f)) throw new SyncError(`schema line ${line}: bad range`); return f; });
    return ft(k, { min, max, res });
  };
  const paren = (n: string): number | null => {
    if (!b.startsWith(n)) return null; const r = b.slice(n.length); if (!r) return 0;
    if (r.startsWith("(") && r.endsWith(")")) { const v = parseInt(r.slice(1, -1).trim(), 10); if (!Number.isNaN(v) && v >= 0) return v; }
    throw new SyncError(`schema line ${line}: bad ${n} size`);
  };
  switch (b) {
    case "bool": return ft("bool");
    case "u8": return ft("uint", { bits: 8 }); case "u16": return ft("uint", { bits: 16 }); case "u32": return ft("uint", { bits: 32 }); case "u64": return ft("uint", { bits: 64 });
    case "i8": return ft("int", { bits: 8 }); case "i16": return ft("int", { bits: 16 }); case "i32": return ft("int", { bits: 32 }); case "i64": return ft("int", { bits: 64 });
    case "f32": return attr === null ? ft("f32") : range("quant");
    case "vec3": return range("vec3"); case "vec2": return range("vec2");
    case "quat": return ft("quat");
  }
  const bits = paren("bits"); if (bits !== null) { if (bits === 0 || bits > 64) throw new SyncError(`schema line ${line}: bits(n) needs 1..=64`); return ft("uint", { bits }); }
  const str = paren("string"); if (str !== null) return ft("string", { maxLen: Math.min(Math.max(str, 1), 65535) });
  const by = paren("bytes"); if (by !== null) return ft("bytes", { maxLen: Math.min(Math.max(by, 1), 65535) });
  throw new SyncError(`schema line ${line}: unknown type ${b}`);
}

// ---------------- values ----------------
/** One field value: boolean, bigint (u8..u64, i8..i64), number (f32/quant), [x,y] / [x,y,z] / [x,y,z,w], string or bytes. */
export type Value = boolean | bigint | number | number[] | string | Uint8Array;

export function zeroValue(t: FieldType): Value {
  switch (t.kind) {
    case "bool": return false; case "uint": case "int": return 0n; case "f32": case "quant": return 0;
    case "vec2": return [0, 0]; case "vec3": return [0, 0, 0]; case "quat": return [0, 0, 0, 1];
    case "string": return ""; case "bytes": return new Uint8Array(0);
  }
}
const QLIM = 0.70710677;
const QRES = (2 * QLIM) / ((1 << 10) - 1);

export function writeField(w: BitWriter, t: FieldType, v: Value): void {
  switch (t.kind) {
    case "bool": w.writeBool(Boolean(v)); break;
    case "uint": w.writeUInt(v as bigint | number, t.bits); break;
    case "int": w.writeInt(v as bigint | number, t.bits); break;
    case "f32": w.writeF32(v as number); break;
    case "quant": w.writeQuantized(v as number, t.min, t.max, t.res); break;
    case "vec2": case "vec3": for (const x of v as number[]) w.writeQuantized(x, t.min, t.max, t.res); break;
    case "quat": {
      const [x, y, z, ww] = v as number[]; const len = Math.sqrt(x * x + y * y + z * z + ww * ww);
      const q = len > 0 ? [x / len, y / len, z / len, ww / len] : [0, 0, 0, 1];
      let largest = 0; for (let i = 1; i < 4; i++) if (Math.abs(q[i]) > Math.abs(q[largest])) largest = i;
      const sign = q[largest] < 0 ? -1 : 1; w.writeUInt(largest, 2);
      for (let i = 0; i < 4; i++) if (i !== largest) w.writeQuantized(q[i] * sign, -QLIM, QLIM, QRES);
      break;
    }
    case "string": { let b: Uint8Array = new TextEncoder().encode(String(v ?? "")); if (b.length > t.maxLen) b = truncateUtf8(b, t.maxLen); w.writeUInt(b.length, lenBits(t.maxLen)); w.writeBytes(b); break; }
    case "bytes": { let b = v as Uint8Array; if (b.length > t.maxLen) b = b.slice(0, t.maxLen); w.writeUInt(b.length, lenBits(t.maxLen)); w.writeBytes(b); break; }
  }
}
function truncateUtf8(b: Uint8Array, max: number): Uint8Array { let n = max; while (n > 0 && (b[n] & 0xc0) === 0x80) n--; return b.slice(0, n); }

export function readField(r: BitReader, t: FieldType): Value {
  switch (t.kind) {
    case "bool": return r.readBool();
    case "uint": return r.readUInt(t.bits);
    case "int": return r.readInt(t.bits);
    case "f32": return r.readF32();
    case "quant": return r.readQuantized(t.min, t.max, t.res);
    case "vec2": return [r.readQuantized(t.min, t.max, t.res), r.readQuantized(t.min, t.max, t.res)];
    case "vec3": return [r.readQuantized(t.min, t.max, t.res), r.readQuantized(t.min, t.max, t.res), r.readQuantized(t.min, t.max, t.res)];
    case "quat": {
      const largest = r.readBits(2); const q = [0, 0, 0, 0]; let sum = 0;
      for (let i = 0; i < 4; i++) if (i !== largest) { q[i] = r.readQuantized(-QLIM, QLIM, QRES); sum += q[i] * q[i]; }
      q[largest] = Math.sqrt(Math.max(0, 1 - sum)); return q;
    }
    case "string": { const n = Number(r.readUInt(lenBits(t.maxLen))); return new TextDecoder().decode(r.readBytes(n)); }
    case "bytes": { const n = Number(r.readUInt(lenBits(t.maxLen))); return r.readBytes(n); }
  }
}

export class ComponentValue {
  constructor(public fields: Value[]) {}
  static zero(c: ComponentDef): ComponentValue { return new ComponentValue(c.fields.map((f) => zeroValue(f.type))); }
  clone(): ComponentValue { return new ComponentValue(this.fields.slice()); }
  get(c: ComponentDef, name: string): Value | undefined { const i = c.fields.findIndex((f) => f.name === name); return i < 0 ? undefined : this.fields[i]; }
  encodeFull(c: ComponentDef, w: BitWriter): void { c.fields.forEach((f, i) => writeField(w, f.type, this.fields[i])); }
  static decodeFull(c: ComponentDef, r: BitReader): ComponentValue { return new ComponentValue(c.fields.map((f) => readField(r, f.type))); }
  static decodeDelta(base: ComponentValue, c: ComponentDef, r: BitReader): ComponentValue {
    const changed = c.fields.map(() => r.readBool()); const o = base.clone();
    changed.forEach((ch, i) => { if (ch) o.fields[i] = readField(r, c.fields[i].type); });
    return o;
  }
}

// ---------------- world ----------------
export interface NetEntity { id: number; owner: number | null; lastTick: number; components: Map<number, ComponentValue> }

export class SyncWorld {
  readonly entities = new Map<number, NetEntity>();
  serverTick = 0; gaps = 0;
  onSpawn?: (e: NetEntity) => void; onChange?: (e: NetEntity) => void; onDespawn?: (e: NetEntity) => void;
  private any = false; private lastDelta = 0; private history = 0;
  constructor(readonly schema: Schema) {}

  get(e: NetEntity, c: ComponentDef | undefined): ComponentValue | undefined { return c ? e.components.get(c.id) : undefined; }

  /** Feeds any sync datagram payload; returns the ack to send back (null when it was not a delta). */
  receive(p: Uint8Array): Uint8Array | null {
    if (p.length < 11 || p[0] !== KIND_DELTA) return null;
    const r = new BitReader(p, 1);
    const tick = r.readBits(32), baseline = r.readBits(32);
    this.serverTick = tick;
    if (!this.any) { this.any = true; this.lastDelta = tick; this.history = 0; }
    else if (tick > this.lastDelta) {
      const sh = tick - this.lastDelta;
      if (baseline !== 0 && baseline - 1 !== this.lastDelta && !this.acked(baseline - 1)) this.gaps++;
      this.history = sh >= 32 ? 0 : ((this.history << sh) | (1 << (sh - 1))) >>> 0; this.lastDelta = tick;
    } else if (tick < this.lastDelta) { const d = this.lastDelta - tick; if (d <= 32) this.history = (this.history | (1 << (d - 1))) >>> 0; }
    const nd = r.readBits(16);
    for (let i = 0; i < nd; i++) { const id = r.readBits(48); const e = this.entities.get(id); if (e) { this.entities.delete(id); this.onDespawn?.(e); } }
    const n = r.readBits(16); const cidBits = this.schema.componentIdBits;
    for (let i = 0; i < n; i++) {
      const id = r.readBits(48); let e = this.entities.get(id); const fresh = !e;
      if (!e) { e = { id, owner: null, lastTick: tick, components: new Map() }; this.entities.set(id, e); }
      e.owner = r.readBool() ? r.readBits(16) : null; e.lastTick = tick;
      const nc = r.readBits(8);
      for (let k = 0; k < nc; k++) {
        const cid = r.readBits(cidBits); const c = this.schema.byId(cid); if (!c) throw new SyncError(`delta: unknown component ${cid}`);
        if (r.readBool()) { e.components.delete(cid); continue; }
        const full = r.readBool();
        e.components.set(cid, full ? ComponentValue.decodeFull(c, r) : ComponentValue.decodeDelta(e.components.get(cid) ?? ComponentValue.zero(c), c, r));
      }
      if (fresh) this.onSpawn?.(e); else this.onChange?.(e);
    }
    return SyncWorld.encodeAck(this.lastDelta, this.history);
  }
  private acked(t: number): boolean { if (t === this.lastDelta) return true; if (t > this.lastDelta) return false; const d = this.lastDelta - t; return d <= 32 && (this.history & (1 << (d - 1))) !== 0; }

  static encodeAck(last: number, bits: number): Uint8Array { return Uint8Array.from([KIND_ACK, last >>> 24, (last >>> 16) & 255, (last >>> 8) & 255, last & 255, bits >>> 24, (bits >>> 16) & 255, (bits >>> 8) & 255, bits & 255]); }

  /** Input packet: the last inputs ending at `firstTick + inputs.length - 1` (send 2-3 for redundancy). */
  static encodeInput(firstTick: number, inputs: Uint8Array[]): Uint8Array {
    let len = 6; for (const i of inputs) len += 1 + Math.min(255, i.length);
    const b = new Uint8Array(len); let o = 0; b[o++] = KIND_INPUT; b[o++] = firstTick >>> 24; b[o++] = (firstTick >>> 16) & 255; b[o++] = (firstTick >>> 8) & 255; b[o++] = firstTick & 255; b[o++] = Math.min(255, inputs.length);
    for (const i of inputs) { const n = Math.min(255, i.length); b[o++] = n; b.set(i.subarray(0, n), o); o += n; }
    return b;
  }

  /** Owner-authoritative input body: full writes of the given components (applied to the actor's avatar). */
  encodeWrites(writes: Array<[ComponentDef, ComponentValue]>): Uint8Array {
    const w = new BitWriter(); w.writeBits(writes.length, 8); const cb = this.schema.componentIdBits;
    for (const [c, v] of writes) { w.writeBits(c.id, cb); v.encodeFull(c, w); }
    return w.finish();
  }
  /** Owner-authoritative ops: write/spawn/despawn/transfer entities you own (see docs/modes.md). */
  encodeOps(ops: SyncOp[]): Uint8Array {
    const w = new BitWriter(); w.writeBits(0xff, 8); w.writeBits(ops.length, 8); const cb = this.schema.componentIdBits;
    const writes = (ws: Array<[ComponentDef, ComponentValue]>) => { w.writeBits(ws.length, 8); for (const [c, v] of ws) { w.writeBits(c.id, cb); v.encodeFull(c, w); } };
    for (const op of ops) {
      switch (op.op) {
        case "write": w.writeBits(0, 8); w.writeBits(op.entity, 48); writes(op.writes); break;
        case "spawn": w.writeBits(1, 8); writes(op.writes); break;
        case "despawn": w.writeBits(2, 8); w.writeBits(op.entity, 48); break;
        case "transfer": w.writeBits(3, 8); w.writeBits(op.entity, 48); w.writeBits(op.to, 16); break;
      }
    }
    return w.finish();
  }
}

// ---------------- interpolation ----------------
/** Remote entity smoothing: keep the last few server samples and read the state `renderTick` (a few ticks
 *  behind `serverTick`) by linear interpolation; snaps when the entity jumped farther than `teleport`. */
export class InterpolationBuffer<T> {
  private buf: Array<{ tick: number; v: T }> = [];
  constructor(private readonly lerp: (a: T, b: T, t: number) => T, private readonly distance?: (a: T, b: T) => number, private readonly teleport = Infinity, private readonly keep = 8) {}
  push(tick: number, v: T): void {
    const last = this.buf[this.buf.length - 1];
    if (last && this.distance && this.distance(last.v, v) > this.teleport) this.buf = [];
    this.buf.push({ tick, v }); while (this.buf.length > this.keep) this.buf.shift();
  }
  sample(renderTick: number): T | undefined {
    if (this.buf.length === 0) return undefined;
    let a = this.buf[0], b = this.buf[this.buf.length - 1];
    for (let i = 0; i + 1 < this.buf.length; i++) if (this.buf[i].tick <= renderTick && this.buf[i + 1].tick >= renderTick) { a = this.buf[i]; b = this.buf[i + 1]; break; }
    let t = b.tick > a.tick ? Math.min(1, Math.max(0, (renderTick - a.tick) / (b.tick - a.tick))) : 1;
    if (renderTick > b.tick) t = 1;
    return this.lerp(a.v, b.v, t);
  }
  clear(): void { this.buf = []; }
}
export const lerpVec = (a: number[], b: number[], t: number): number[] => a.map((x, i) => x + (b[i] - x) * t);
export const distVec = (a: number[], b: number[]): number => Math.hypot(...a.map((x, i) => x - b[i]));

// ---------------- client glue ----------------
import { Channel } from "./packet.js";
import type { Client } from "./index.js";

/** Drives a `SyncWorld` from a joined sync-room `Client`: decodes deltas, acks them, advances a render
 *  tick `interpolationTicks` behind the server, and stamps/sends owner writes or raw inputs. */
export class SyncSession {
  readonly world: SyncWorld;
  renderTick = 0; private lastInputTick = 0; private recent: Uint8Array[] = []; private lastDeltaAt = 0;
  constructor(readonly client: Client, schema: Schema | string, readonly tickHz = 60, readonly interpolationTicks = 2) {
    this.world = new SyncWorld(typeof schema === "string" ? Schema.parse(schema) : schema);
    client.on("data", (_ch: Channel, _tick: number, payload: Uint8Array) => {
      const ack = this.world.receive(payload);
      if (ack) { client.sendData(Channel.Unreliable, ack); this.renderTick = Math.max(this.renderTick, this.world.serverTick - this.interpolationTicks); this.lastDeltaAt = performance.now(); }
    });
  }
  /** Call once per animation frame: advances the render tick between deltas. */
  advance(dtSeconds: number): number { if (this.lastDeltaAt > 0) this.renderTick += dtSeconds * this.tickHz; return this.renderTick; }
  /** Raw input for server logic (`props.logic`); returns the tick it was stamped with. Resent with the next two inputs. */
  sendInput(body: Uint8Array): number {
    const tick = Math.max(this.world.serverTick + 2, this.lastInputTick + 1); this.lastInputTick = tick;
    this.recent.push(body); while (this.recent.length > 3) this.recent.shift();
    this.client.sendData(Channel.Unreliable, SyncWorld.encodeInput(tick - (this.recent.length - 1), this.recent));
    return tick;
  }
  /** Owner-authoritative writes of your avatar's components (rooms without `logic`). */
  sendWrites(writes: Array<[ComponentDef, ComponentValue]>): number { return this.sendInput(this.world.encodeWrites(writes)); }
  /** Owner-authoritative ops on entities you own: at most 32 spawned per actor, owner is in every delta. */
  sendOps(ops: SyncOp[]): number { return this.sendInput(this.world.encodeOps(ops)); }
  spawn(writes: Array<[ComponentDef, ComponentValue]> = []): number { return this.sendOps([{ op: "spawn", writes }]); }
  despawn(entity: number): number { return this.sendOps([{ op: "despawn", entity }]); }
  /** Hands an entity you own to another actor in the room (Fusion: RequestStateAuthority, but owner-initiated). */
  transfer(entity: number, to: number): number { return this.sendOps([{ op: "transfer", entity, to }]); }
  writeEntity(entity: number, writes: Array<[ComponentDef, ComponentValue]>): number { return this.sendOps([{ op: "write", entity, writes }]); }
}
