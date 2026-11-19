// Sync rooms for the browser: schema IDL, MSB-first bit codec, delta decoding, acks, owner inputs and
// an interpolation buffer: a port of the managed C# core (mirrors net-wire::{bits,schema,value}).

export type SyncOp =
  | {
      op: "write";
      entity: number;
      writes: Array<[ComponentDef, ComponentValue]>;
    }
  | { op: "spawn"; writes: Array<[ComponentDef, ComponentValue]> }
  | {
      op: "spawn_tagged";
      tag: number;
      writes: Array<[ComponentDef, ComponentValue]>;
    }
  | { op: "despawn"; entity: number }
  | { op: "transfer"; entity: number; to: number }
  | { op: "set_policy"; entity: number; policy: OwnerPolicy }
  | { op: "request"; entity: number }
  | { op: "release"; entity: number };

/** Per-entity ownership policy: what the cell does when the owner leaves and who may take the entity. */
export enum OwnerPolicy {
  DestroyOnLeave = 0,
  ToMaster = 1,
  Orphan = 2,
  Dynamic = 3,
}

export const KIND_INPUT = 0x10;
export const KIND_DELTA = 0x11;
export const KIND_RESYNC = 0x12;
export const KIND_ACK = 0x13;
export const KIND_SPAWN_ANSWER = 0x14;
export const KIND_INPUT_STATS = 0x25;

export class SyncError extends Error {}

// ---------------- bits ----------------
export class BitWriter {
  private buf: number[] = [];
  private scratch = 0n;
  private n = 0;
  bitLen = 0;
  writeBits(value: bigint | number, nbits: number): void {
    if (nbits === 0) {
      return;
    }
    let x = BigInt.asUintN(64, BigInt(value));
    if (nbits < 64) {
      x &= (1n << BigInt(nbits)) - 1n;
    }
    this.bitLen += nbits;
    let left = nbits;
    while (left > 0) {
      const take = Math.min(left, 64 - this.n);
      const part = (x >> BigInt(left - take)) & ((1n << BigInt(take)) - 1n);
      this.scratch =
        take === 64
          ? part
          : ((this.scratch << BigInt(take)) | part) & ((1n << 64n) - 1n);
      this.n += take;
      left -= take;
      while (this.n >= 8) {
        this.buf.push(Number((this.scratch >> BigInt(this.n - 8)) & 0xffn));
        this.n -= 8;
      }
    }
  }
  writeBool(value: boolean): void {
    this.writeBits(value ? 1 : 0, 1);
  }
  writeUInt(value: bigint | number, nbits: number): void {
    const max = nbits >= 64 ? (1n << 64n) - 1n : (1n << BigInt(nbits)) - 1n;
    const x = BigInt(value);
    this.writeBits(x > max ? max : x, nbits);
  }
  writeInt(value: bigint | number, nbits: number): void {
    const x = BigInt.asIntN(64, BigInt(value));
    this.writeUInt(BigInt.asUintN(64, (x << 1n) ^ (x >> 63n)), nbits);
  }
  writeQuantized(value: number, min: number, max: number, res: number): void {
    const { bits } = quantBits(min, max, res);
    const quantized = Math.floor(
      (Math.min(Math.max(value, min), max) - min) / res + 0.5,
    );
    this.writeUInt(quantized, bits);
  }
  writeF32(value: number): void {
    const dv = new DataView(new ArrayBuffer(4));
    dv.setFloat32(0, value);
    this.writeBits(dv.getUint32(0), 32);
  }
  writeBytes(bytes: Uint8Array): void {
    for (const x of bytes) {
      this.writeBits(x, 8);
    }
  }
  finish(): Uint8Array {
    const bytes = this.buf.slice();
    if (this.n > 0) {
      bytes.push(Number((this.scratch << BigInt(8 - this.n)) & 0xffn));
    }
    return Uint8Array.from(bytes);
  }
}

export class BitReader {
  private pos = 0;
  private readonly bits: number;
  constructor(
    private readonly bytes: Uint8Array,
    private readonly off = 0,
  ) {
    this.bits = (bytes.length - off) * 8;
  }
  get bitsLeft(): number {
    return this.bits - this.pos;
  }
  readBig(nbits: number): bigint {
    if (nbits > this.bitsLeft) {
      throw new SyncError("delta truncated");
    }
    let value = 0n;
    for (let i = 0; i < nbits; i++) {
      const position = this.pos + i;
      value =
        (value << 1n) |
        BigInt(
          (this.bytes[this.off + (position >> 3)] >> (7 - (position & 7))) & 1,
        );
    }
    this.pos += nbits;
    return value;
  }
  /** Up to 53 bits as a number. */
  readBits(nbits: number): number {
    if (nbits > 53) {
      throw new SyncError("readBits > 53");
    }
    return Number(this.readBig(nbits));
  }
  readBool(): boolean {
    return this.readBits(1) === 1;
  }
  readUInt(nbits: number): bigint {
    return this.readBig(nbits);
  }
  readInt(nbits: number): bigint {
    const zz = this.readBig(nbits);
    return (zz >> 1n) ^ -(zz & 1n);
  }
  readQuantized(min: number, max: number, res: number): number {
    const { bits } = quantBits(min, max, res);
    return min + Number(this.readBig(bits)) * res;
  }
  readF32(): number {
    const dv = new DataView(new ArrayBuffer(4));
    dv.setUint32(0, this.readBits(32));
    return dv.getFloat32(0);
  }
  readBytes(count: number): Uint8Array {
    const out = new Uint8Array(count);
    for (let i = 0; i < count; i++) {
      out[i] = this.readBits(8);
    }
    return out;
  }
}

export function quantBits(
  min: number,
  max: number,
  res: number,
): { bits: number; steps: number } {
  const steps = Math.max(1, Math.ceil((max - min) / res));
  return { bits: Math.max(1, bitLength(steps)), steps };
}
function bitLength(value: number): number {
  let count = 0;
  let x = Math.floor(value);
  while (x > 0) {
    count++;
    x = Math.floor(x / 2);
  }
  return count;
}
export function lenBits(max: number): number {
  return Math.max(1, bitLength(max));
}

// ---------------- schema ----------------
export type FieldKind =
  | "bool"
  | "uint"
  | "int"
  | "f32"
  | "quant"
  | "vec2"
  | "vec3"
  | "quat"
  | "string"
  | "bytes";
export interface FieldType {
  kind: FieldKind;
  bits: number;
  min: number;
  max: number;
  res: number;
  maxLen: number;
}
export interface Field {
  name: string;
  type: FieldType;
}
export interface ComponentDef {
  id: number;
  name: string;
  ownerOnly: boolean;
  global: boolean;
  fields: Field[];
}

export class Schema {
  components: ComponentDef[] = [];
  byId(id: number): ComponentDef | undefined {
    return this.components[id];
  }
  byName(name: string): ComponentDef | undefined {
    return this.components.find((component) => component.name === name);
  }
  get componentIdBits(): number {
    return Math.max(1, bitLength(Math.max(2, this.components.length) - 1));
  }
  indexOf(component: ComponentDef, field: string): number {
    return component.fields.findIndex((candidate) => candidate.name === field);
  }

  static parse(text: string): Schema {
    const schema = new Schema();
    let cur: ComponentDef | null = null;
    let line = 0;
    for (const raw of text.split("\n")) {
      line++;
      let source = raw;
      const commentStart = source.indexOf("//");
      if (commentStart >= 0) {
        source = source.slice(0, commentStart);
      }
      source = source.trim();
      if (!source) {
        continue;
      }
      if (source.startsWith("component ")) {
        if (cur) {
          throw new SyncError(`schema line ${line}: nested component`);
        }
        const rest = source.slice(10).trim();
        if (!rest.endsWith("{")) {
          throw new SyncError(`schema line ${line}: expected '{'`);
        }
        const parts = rest
          .slice(0, -1)
          .trim()
          .split(/[ \t]+/)
          .filter(Boolean);
        cur = {
          id: schema.components.length,
          name: parts[0],
          ownerOnly: false,
          global: false,
          fields: [],
        };
        for (const part of parts.slice(1)) {
          if (part === "@owner_only") {
            cur.ownerOnly = true;
          } else if (part === "@global") {
            cur.global = true;
          } else {
            throw new SyncError(`schema line ${line}: unknown flag ${part}`);
          }
        }
        continue;
      }
      if (source === "}") {
        if (!cur) {
          throw new SyncError(`schema line ${line}: stray '}'`);
        }
        schema.components.push(cur);
        cur = null;
        continue;
      }
      if (!cur) {
        throw new SyncError(`schema line ${line}: field outside component`);
      }
      const colon = source.indexOf(":");
      if (colon < 0) {
        throw new SyncError(`schema line ${line}: expected name: type`);
      }
      const name = source.slice(0, colon).trim();
      let ty = source.slice(colon + 1).trim();
      let attr: string | null = null;
      const at = ty.indexOf("@");
      if (at >= 0) {
        attr = ty.slice(at + 1).trim();
        ty = ty.slice(0, at).trim();
      }
      cur.fields.push({ name, type: parseType(ty, attr, line) });
    }
    if (cur) {
      throw new SyncError("schema: unterminated component");
    }
    return schema;
  }
}

function ft(kind: FieldKind, overrides: Partial<FieldType> = {}): FieldType {
  return { kind, bits: 0, min: 0, max: 0, res: 0, maxLen: 0, ...overrides };
}
function parseType(base: string, attr: string | null, line: number): FieldType {
  const range = (kind: FieldKind): FieldType => {
    if (!attr || !attr.startsWith("range(") || !attr.endsWith(")")) {
      throw new SyncError(
        `schema line ${line}: ${base} needs @range(min, max, res)`,
      );
    }
    const parts = attr.slice(6, -1).split(",");
    if (parts.length !== 3) {
      throw new SyncError(`schema line ${line}: bad range`);
    }
    const [min, max, res] = parts.map((text) => {
      const parsed = parseFloat(text.trim());
      if (Number.isNaN(parsed)) {
        throw new SyncError(`schema line ${line}: bad range`);
      }
      return parsed;
    });
    return ft(kind, { min, max, res });
  };
  const paren = (name: string): number | null => {
    if (!base.startsWith(name)) {
      return null;
    }
    const rest = base.slice(name.length);
    if (!rest) {
      return 0;
    }
    if (rest.startsWith("(") && rest.endsWith(")")) {
      const value = parseInt(rest.slice(1, -1).trim(), 10);
      if (!Number.isNaN(value) && value >= 0) {
        return value;
      }
    }
    throw new SyncError(`schema line ${line}: bad ${name} size`);
  };
  switch (base) {
    case "bool":
      return ft("bool");
    case "u8":
      return ft("uint", { bits: 8 });
    case "u16":
      return ft("uint", { bits: 16 });
    case "u32":
      return ft("uint", { bits: 32 });
    case "u64":
      return ft("uint", { bits: 64 });
    case "i8":
      return ft("int", { bits: 8 });
    case "i16":
      return ft("int", { bits: 16 });
    case "i32":
      return ft("int", { bits: 32 });
    case "i64":
      return ft("int", { bits: 64 });
    case "f32":
      return attr === null ? ft("f32") : range("quant");
    case "vec3":
      return range("vec3");
    case "vec2":
      return range("vec2");
    case "quat":
      return ft("quat");
  }
  const bits = paren("bits");
  if (bits !== null) {
    if (bits === 0 || bits > 64) {
      throw new SyncError(`schema line ${line}: bits(n) needs 1..=64`);
    }
    return ft("uint", { bits });
  }
  const str = paren("string");
  if (str !== null) {
    return ft("string", { maxLen: Math.min(Math.max(str, 1), 65535) });
  }
  const by = paren("bytes");
  if (by !== null) {
    return ft("bytes", { maxLen: Math.min(Math.max(by, 1), 65535) });
  }
  throw new SyncError(`schema line ${line}: unknown type ${base}`);
}

// ---------------- values ----------------
/** One field value: boolean, bigint (u8..u64, i8..i64), number (f32/quant), [x,y] / [x,y,z] / [x,y,z,w], string or bytes. */
export type Value = boolean | bigint | number | number[] | string | Uint8Array;

export function zeroValue(type: FieldType): Value {
  switch (type.kind) {
    case "bool":
      return false;
    case "uint":
    case "int":
      return 0n;
    case "f32":
    case "quant":
      return 0;
    case "vec2":
      return [0, 0];
    case "vec3":
      return [0, 0, 0];
    case "quat":
      return [0, 0, 0, 1];
    case "string":
      return "";
    case "bytes":
      return new Uint8Array(0);
  }
}
const QLIM = 0.70710677;
const QRES = (2 * QLIM) / ((1 << 10) - 1);

export function writeField(
  writer: BitWriter,
  type: FieldType,
  value: Value,
): void {
  switch (type.kind) {
    case "bool":
      writer.writeBool(Boolean(value));
      break;
    case "uint":
      writer.writeUInt(value as bigint | number, type.bits);
      break;
    case "int":
      writer.writeInt(value as bigint | number, type.bits);
      break;
    case "f32":
      writer.writeF32(value as number);
      break;
    case "quant":
      writer.writeQuantized(value as number, type.min, type.max, type.res);
      break;
    case "vec2":
    case "vec3":
      for (const x of value as number[]) {
        writer.writeQuantized(x, type.min, type.max, type.res);
      }
      break;
    case "quat": {
      const [x, y, z, ww] = value as number[];
      const len = Math.sqrt(x * x + y * y + z * z + ww * ww);
      const normalized =
        len > 0 ? [x / len, y / len, z / len, ww / len] : [0, 0, 0, 1];
      let largest = 0;
      for (let i = 1; i < 4; i++) {
        if (Math.abs(normalized[i]) > Math.abs(normalized[largest])) {
          largest = i;
        }
      }
      const sign = normalized[largest] < 0 ? -1 : 1;
      writer.writeUInt(largest, 2);
      for (let i = 0; i < 4; i++) {
        if (i !== largest) {
          writer.writeQuantized(normalized[i] * sign, -QLIM, QLIM, QRES);
        }
      }
      break;
    }
    case "string": {
      let bytes: Uint8Array = new TextEncoder().encode(String(value ?? ""));
      if (bytes.length > type.maxLen) {
        bytes = truncateUtf8(bytes, type.maxLen);
      }
      writer.writeUInt(bytes.length, lenBits(type.maxLen));
      writer.writeBytes(bytes);
      break;
    }
    case "bytes": {
      let bytes = value as Uint8Array;
      if (bytes.length > type.maxLen) {
        bytes = bytes.slice(0, type.maxLen);
      }
      writer.writeUInt(bytes.length, lenBits(type.maxLen));
      writer.writeBytes(bytes);
      break;
    }
  }
}
function truncateUtf8(bytes: Uint8Array, max: number): Uint8Array {
  let count = max;
  while (count > 0 && (bytes[count] & 0xc0) === 0x80) {
    count--;
  }
  return bytes.slice(0, count);
}

export function readField(reader: BitReader, type: FieldType): Value {
  switch (type.kind) {
    case "bool":
      return reader.readBool();
    case "uint":
      return reader.readUInt(type.bits);
    case "int":
      return reader.readInt(type.bits);
    case "f32":
      return reader.readF32();
    case "quant":
      return reader.readQuantized(type.min, type.max, type.res);
    case "vec2":
      return [
        reader.readQuantized(type.min, type.max, type.res),
        reader.readQuantized(type.min, type.max, type.res),
      ];
    case "vec3":
      return [
        reader.readQuantized(type.min, type.max, type.res),
        reader.readQuantized(type.min, type.max, type.res),
        reader.readQuantized(type.min, type.max, type.res),
      ];
    case "quat": {
      const largest = reader.readBits(2);
      const normalized = [0, 0, 0, 0];
      let sum = 0;
      for (let i = 0; i < 4; i++) {
        if (i !== largest) {
          normalized[i] = reader.readQuantized(-QLIM, QLIM, QRES);
          sum += normalized[i] * normalized[i];
        }
      }
      normalized[largest] = Math.sqrt(Math.max(0, 1 - sum));
      return normalized;
    }
    case "string": {
      const count = Number(reader.readUInt(lenBits(type.maxLen)));
      return new TextDecoder().decode(reader.readBytes(count));
    }
    case "bytes": {
      const count = Number(reader.readUInt(lenBits(type.maxLen)));
      return reader.readBytes(count);
    }
  }
}

export class ComponentValue {
  constructor(public fields: Value[]) {}
  static zero(component: ComponentDef): ComponentValue {
    return new ComponentValue(
      component.fields.map((field) => zeroValue(field.type)),
    );
  }
  clone(): ComponentValue {
    return new ComponentValue(this.fields.slice());
  }
  get(component: ComponentDef, name: string): Value | undefined {
    const index = component.fields.findIndex((field) => field.name === name);
    return index < 0 ? undefined : this.fields[index];
  }
  encodeFull(component: ComponentDef, writer: BitWriter): void {
    component.fields.forEach((field, i) =>
      writeField(writer, field.type, this.fields[i]),
    );
  }
  static decodeFull(
    component: ComponentDef,
    reader: BitReader,
  ): ComponentValue {
    return new ComponentValue(
      component.fields.map((field) => readField(reader, field.type)),
    );
  }
  static decodeDelta(
    base: ComponentValue,
    component: ComponentDef,
    reader: BitReader,
  ): ComponentValue {
    const changed = component.fields.map(() => reader.readBool());
    const copy = base.clone();
    changed.forEach((ch, i) => {
      if (ch) {
        copy.fields[i] = readField(reader, component.fields[i].type);
      }
    });
    return copy;
  }
}

// ---------------- world ----------------
export interface NetEntity {
  id: number;
  owner: number | null;
  lastTick: number;
  components: Map<number, ComponentValue>;
}

export class SyncWorld {
  readonly entities = new Map<number, NetEntity>();
  serverTick = 0;
  gaps = 0;
  onSpawn?: (entity: NetEntity) => void;
  onChange?: (entity: NetEntity) => void;
  onDespawn?: (entity: NetEntity) => void;
  private any = false;
  private lastDelta = 0;
  private history = 0;
  constructor(readonly schema: Schema) {}

  get(
    entity: NetEntity,
    component: ComponentDef | undefined,
  ): ComponentValue | undefined {
    return component ? entity.components.get(component.id) : undefined;
  }

  /** Feeds any sync datagram payload; returns the ack to send back (null when it was not a delta). */
  receive(packet: Uint8Array): Uint8Array | null {
    if (packet.length < 11 || packet[0] !== KIND_DELTA) {
      return null;
    }
    const reader = new BitReader(packet, 1);
    const tick = reader.readBits(32);
    const baseline = reader.readBits(32);
    this.serverTick = tick;
    if (!this.any) {
      this.any = true;
      this.lastDelta = tick;
      this.history = 0;
    } else if (tick > this.lastDelta) {
      const sh = tick - this.lastDelta;
      if (
        baseline !== 0 &&
        baseline - 1 !== this.lastDelta &&
        !this.acked(baseline - 1)
      ) {
        this.gaps++;
      }
      this.history =
        sh >= 32 ? 0 : ((this.history << sh) | (1 << (sh - 1))) >>> 0;
      this.lastDelta = tick;
    } else if (tick < this.lastDelta) {
      const behind = this.lastDelta - tick;
      if (behind <= 32) {
        this.history = (this.history | (1 << (behind - 1))) >>> 0;
      }
    }
    const nd = reader.readBits(16);
    for (let i = 0; i < nd; i++) {
      const id = reader.readBits(48);
      const entity = this.entities.get(id);
      if (entity) {
        this.entities.delete(id);
        this.onDespawn?.(entity);
      }
    }
    const count = reader.readBits(16);
    const cidBits = this.schema.componentIdBits;
    for (let i = 0; i < count; i++) {
      const id = reader.readBits(48);
      let entity = this.entities.get(id);
      const fresh = !entity;
      if (!entity) {
        entity = { id, owner: null, lastTick: tick, components: new Map() };
        this.entities.set(id, entity);
      }
      entity.owner = reader.readBool() ? reader.readBits(16) : null;
      entity.lastTick = tick;
      const nc = reader.readBits(8);
      for (let k = 0; k < nc; k++) {
        const cid = reader.readBits(cidBits);
        const component = this.schema.byId(cid);
        if (!component) {
          throw new SyncError(`delta: unknown component ${cid}`);
        }
        if (reader.readBool()) {
          entity.components.delete(cid);
          continue;
        }
        const full = reader.readBool();
        entity.components.set(
          cid,
          full
            ? ComponentValue.decodeFull(component, reader)
            : ComponentValue.decodeDelta(
                entity.components.get(cid) ?? ComponentValue.zero(component),
                component,
                reader,
              ),
        );
      }
      if (fresh) {
        this.onSpawn?.(entity);
      } else {
        this.onChange?.(entity);
      }
    }
    return SyncWorld.encodeAck(this.lastDelta, this.history);
  }
  private acked(fraction: number): boolean {
    if (fraction === this.lastDelta) {
      return true;
    }
    if (fraction > this.lastDelta) {
      return false;
    }
    const behind = this.lastDelta - fraction;
    return behind <= 32 && (this.history & (1 << (behind - 1))) !== 0;
  }

  static encodeAck(last: number, bits: number): Uint8Array {
    return Uint8Array.from([
      KIND_ACK,
      last >>> 24,
      (last >>> 16) & 255,
      (last >>> 8) & 255,
      last & 255,
      bits >>> 24,
      (bits >>> 16) & 255,
      (bits >>> 8) & 255,
      bits & 255,
    ]);
  }

  /** Input packet: the last inputs ending at `firstTick + inputs.length - 1` (send 2-3 for redundancy). */
  static encodeInput(firstTick: number, inputs: Uint8Array[]): Uint8Array {
    let len = 6;
    for (const input of inputs) {
      len += 1 + Math.min(255, input.length);
    }
    const bytes = new Uint8Array(len);
    let offset = 0;
    bytes[offset++] = KIND_INPUT;
    bytes[offset++] = firstTick >>> 24;
    bytes[offset++] = (firstTick >>> 16) & 255;
    bytes[offset++] = (firstTick >>> 8) & 255;
    bytes[offset++] = firstTick & 255;
    bytes[offset++] = Math.min(255, inputs.length);
    for (const input of inputs) {
      const count = Math.min(255, input.length);
      bytes[offset++] = count;
      bytes.set(input.subarray(0, count), offset);
      offset += count;
    }
    return bytes;
  }

  /** Owner-authoritative input body: full writes of the given components (applied to the actor's avatar). */
  encodeWrites(writes: Array<[ComponentDef, ComponentValue]>): Uint8Array {
    const writer = new BitWriter();
    writer.writeBits(writes.length, 8);
    const cb = this.schema.componentIdBits;
    for (const [component, value] of writes) {
      writer.writeBits(component.id, cb);
      value.encodeFull(component, writer);
    }
    return writer.finish();
  }
  /** Owner-authoritative ops: write/spawn/despawn/transfer entities you own (see docs/modes.md). */
  encodeOps(ops: SyncOp[]): Uint8Array {
    const writer = new BitWriter();
    writer.writeBits(0xff, 8);
    writer.writeBits(ops.length, 8);
    const cb = this.schema.componentIdBits;
    const writes = (ws: Array<[ComponentDef, ComponentValue]>) => {
      writer.writeBits(ws.length, 8);
      for (const [component, value] of ws) {
        writer.writeBits(component.id, cb);
        value.encodeFull(component, writer);
      }
    };
    for (const op of ops) {
      switch (op.op) {
        case "write":
          writer.writeBits(0, 8);
          writer.writeBits(op.entity, 48);
          writes(op.writes);
          break;
        case "spawn":
          writer.writeBits(1, 8);
          writes(op.writes);
          break;
        case "spawn_tagged":
          writer.writeBits(7, 8);
          writer.writeBits(op.tag, 32);
          writes(op.writes);
          break;
        case "despawn":
          writer.writeBits(2, 8);
          writer.writeBits(op.entity, 48);
          break;
        case "set_policy":
          writer.writeBits(4, 8);
          writer.writeBits(op.entity, 48);
          writer.writeBits(op.policy, 8);
          break;
        case "request":
          writer.writeBits(5, 8);
          writer.writeBits(op.entity, 48);
          break;
        case "release":
          writer.writeBits(6, 8);
          writer.writeBits(op.entity, 48);
          break;
        case "transfer":
          writer.writeBits(3, 8);
          writer.writeBits(op.entity, 48);
          writer.writeBits(op.to, 16);
          break;
      }
    }
    return writer.finish();
  }
}

// ---------------- interpolation ----------------
/** Remote entity smoothing: keep the last few server samples and read the state `renderTick` (a few ticks
 *  behind `serverTick`) by linear interpolation; snaps when the entity jumped farther than `teleport`. */
export class InterpolationBuffer<T> {
  private buf: Array<{ tick: number; v: T }> = [];
  constructor(
    private readonly lerp: (from: T, to: T, fraction: number) => T,
    private readonly distance?: (from: T, to: T) => number,
    private readonly teleport = Infinity,
    private readonly keep = 8,
  ) {}
  push(tick: number, value: T): void {
    const last = this.buf[this.buf.length - 1];
    if (last && this.distance && this.distance(last.v, value) > this.teleport) {
      this.buf = [];
    }
    this.buf.push({ tick, v: value });
    while (this.buf.length > this.keep) {
      this.buf.shift();
    }
  }
  sample(renderTick: number): T | undefined {
    if (this.buf.length === 0) {
      return undefined;
    }
    let older = this.buf[0];
    let newer = this.buf[this.buf.length - 1];
    for (let i = 0; i + 1 < this.buf.length; i++) {
      if (
        this.buf[i].tick <= renderTick &&
        this.buf[i + 1].tick >= renderTick
      ) {
        older = this.buf[i];
        newer = this.buf[i + 1];
        break;
      }
    }
    let fraction =
      newer.tick > older.tick
        ? Math.min(
            1,
            Math.max(0, (renderTick - older.tick) / (newer.tick - older.tick)),
          )
        : 1;
    if (renderTick > newer.tick) {
      fraction = 1;
    }
    return this.lerp(older.v, newer.v, fraction);
  }
  clear(): void {
    this.buf = [];
  }
}
export const lerpVec = (
  from: number[],
  to: number[],
  fraction: number,
): number[] => from.map((x, i) => x + (to[i] - x) * fraction);
export const distVec = (from: number[], to: number[]): number =>
  Math.hypot(...from.map((x, i) => x - to[i]));

// ---------------- client glue ----------------
import { MarginControl } from "./margin.js";
import { Channel } from "./packet.js";
import type { Client } from "./index.js";

// The room drops inputs more than 64 ticks ahead of it.
const MAX_LEAD_TICKS = 60;
// Frames an input rides in: its own and the next ones, as in lockstep.
const INPUT_REDUNDANCY = 3;

/** Drives a `SyncWorld` from a joined sync-room `Client`: decodes deltas, acks them, advances a render
 *  tick `interpolationTicks` behind the server, and stamps/sends owner writes or raw inputs. */
/** The room's answer to `SyncSession.spawn`: the entity it made for `tag`, or why it made none.
 *  `tag` 0 names the avatar (`Client.avatar`). */
export interface SpawnAnswer {
  tag: number;
  /** `refused`: the actor's entity limit, or the room is full. `invalid`: the room could not
   *  read the spawn. */
  status: "created" | "refused" | "invalid";
  /** The entity as deltas name it. Read it only when created: 0 is an entity too. */
  entity: number;
  /** The tick the room ran the spawn at. */
  tick: number;
}

export class SyncSession {
  readonly world: SyncWorld;
  renderTick = 0;
  private lastInputTick = 0;
  private recent: Uint8Array[] = [];
  /** How many repeats the last frame is owed and the room tick it last went out on. Each input
   *  rides in `INPUT_REDUNDANCY` frames; with no input after it, only repeats of its own frame
   *  carry it. */
  private tail = { left: 0, sentAt: 0 };
  private lastDeltaAt = 0;
  private synced = false;
  private held: { body: Uint8Array; keep: boolean }[] = [];
  private lastPingAt = 0;
  private readonly margin = new MarginControl();
  /** Ticks the room's margin reports have added to the input lead (negative: taken off it). */
  private leadAdjust = 0;
  private nextTag = 1;
  /** Tagged spawns the room has not answered; `tick` is the one the latest input was stamped with. */
  private unanswered: { tag: number; body: Uint8Array; tick: number }[] = [];
  /** An answer to `spawn`. It and the delta that carries the entity come in either order. */
  onSpawnAnswer?: (answer: SpawnAnswer) => void;
  constructor(
    readonly client: Client,
    schema: Schema | string,
    readonly tickHz = 60,
    readonly interpolationTicks = 2,
  ) {
    this.world = new SyncWorld(
      typeof schema === "string" ? Schema.parse(schema) : schema,
    );
    client.on("data", (_ch: Channel, _tick: number, payload: Uint8Array) => {
      if (payload.length >= 7 && payload[0] === KIND_INPUT_STATS) {
        const view = new DataView(payload.buffer, payload.byteOffset);
        this.leadAdjust += this.margin.adjust(
          view.getUint32(1),
          view.getInt8(5),
          this.lastInputTick,
        );
        return;
      }
      if (payload.length === 18 && payload[0] === KIND_SPAWN_ANSWER) {
        this.takeSpawnAnswer(payload);
        return;
      }
      const ack = this.world.receive(payload);
      if (ack) {
        client.sendData(Channel.Unreliable, ack);
        this.renderTick = Math.max(
          this.renderTick,
          this.world.serverTick - this.interpolationTicks,
        );
        this.lastDeltaAt = performance.now();
        this.synced = true;
        if (this.lastDeltaAt - this.lastPingAt >= 1000) {
          this.ping();
        }
        this.pump();
      }
    });
    this.ping();
  }
  // An answer to a spawn already answered (the room answers every repeat) is not reported again.
  private takeSpawnAnswer(payload: Uint8Array): void {
    const view = new DataView(payload.buffer, payload.byteOffset);
    const tag = view.getUint32(1);
    const entity = Number(view.getBigUint64(6));
    if (tag !== 0) {
      const left = this.unanswered.filter((spawn) => spawn.tag !== tag);
      if (left.length === this.unanswered.length) {
        return;
      }
      this.unanswered = left;
    }
    this.onSpawnAnswer?.({
      tag,
      status: (["created", "refused"] as const)[view.getUint8(5)] ?? "invalid",
      entity,
      tick: view.getUint32(14),
    });
  }
  private ping(): void {
    this.lastPingAt = performance.now();
    this.client.ping();
  }
  /** Call once per animation frame: advances the render tick between deltas. */
  advance(dtSeconds: number): number {
    if (this.lastDeltaAt > 0) {
      this.renderTick += dtSeconds * this.tickHz;
    }
    return this.renderTick;
  }
  /** Raw input for server logic (`props.logic`); returns the tick it is stamped with. Resent with the next two inputs.
   *  The room takes one input per tick: call this every frame, the newest input of a tick is the one sent. */
  sendInput(body: Uint8Array): number {
    return this.queue(body, false);
  }
  /** Owner-authoritative writes of your avatar's components (rooms without `logic`). */
  sendWrites(writes: Array<[ComponentDef, ComponentValue]>): number {
    return this.queue(this.world.encodeWrites(writes), false);
  }
  /** Owner-authoritative ops on entities you own: at most 32 spawned per actor, owner is in every delta. */
  sendOps(ops: SyncOp[]): number {
    return this.queue(this.world.encodeOps(ops), true);
  }
  // The room drops an input whose tick it has already simulated and applies one stamped far ahead that much
  // later. So an input is stamped ahead of the last delta by the round trip plus two ticks, and no further:
  // the rest waits for the next delta, and everything waits for the first one, which tells the room's tick.
  // Waiting writes and raw inputs are replaced by newer ones, ops are kept.
  private queue(body: Uint8Array, keep: boolean): number {
    const tail = this.held[this.held.length - 1];
    if (tail && !tail.keep && !keep) {
      tail.body = body;
    } else {
      this.held.push({ body, keep });
    }
    this.pump();
    return this.lastInputTick + this.held.length;
  }
  private pump(): void {
    if (!this.synced) {
      return;
    }
    // the round trip gives the first guess; the room's margin reports correct it
    const estimate =
      2 +
      Math.min(
        MAX_LEAD_TICKS,
        Math.ceil((this.client.rttMs * this.tickHz) / 1000),
      );
    const lead = Math.min(
      MAX_LEAD_TICKS + 2,
      Math.max(1, estimate + this.leadAdjust),
    );
    this.leadAdjust = lead - estimate;
    // a tagged spawn the room passed half a second ago with no answer was lost; the room makes
    // nothing new out of a tag it knows
    for (const spawn of this.unanswered) {
      if (this.world.serverTick > spawn.tick + this.tickHz / 2) {
        this.held.push({ body: spawn.body, keep: true });
        spawn.tick = Math.max(
          this.world.serverTick + lead,
          this.lastInputTick + this.held.length,
        );
      }
    }
    let sent = false;
    while (this.held.length) {
      const tick = Math.max(
        this.world.serverTick + lead,
        this.lastInputTick + 1,
      );
      if (tick > this.world.serverTick + lead + 1) {
        break;
      }
      // a frame carries inputs for consecutive ticks, so earlier ones are resent only without a gap
      if (tick !== this.lastInputTick + 1) {
        this.recent = [];
      }
      this.lastInputTick = tick;
      this.recent.push(this.held.shift()!.body);
      while (this.recent.length > INPUT_REDUNDANCY) {
        this.recent.shift();
      }
      this.client.sendData(
        Channel.Unreliable,
        SyncWorld.encodeInput(tick - (this.recent.length - 1), this.recent),
      );
      this.tail = { left: INPUT_REDUNDANCY - 1, sentAt: this.world.serverTick };
      sent = true;
    }
    if (
      !sent &&
      this.tail.left > 0 &&
      this.tail.sentAt !== this.world.serverTick &&
      this.recent.length
    ) {
      this.tail = { left: this.tail.left - 1, sentAt: this.world.serverTick };
      this.client.sendData(
        Channel.Unreliable,
        SyncWorld.encodeInput(
          this.lastInputTick - (this.recent.length - 1),
          this.recent,
        ),
      );
    }
  }
  /** Spawns an entity you own and returns its tag: the room answers with the entity it made
   *  (`onSpawnAnswer`). The spawn is repeated until the room answers. */
  spawn(writes: Array<[ComponentDef, ComponentValue]> = []): number {
    const firstFree = this.client.joined?.next_spawn_tag;
    if (firstFree === undefined) {
      throw new SyncError("spawn: the room does not answer tagged spawns");
    }
    const tag = Math.max(this.nextTag, firstFree);
    const body = this.world.encodeOps([{ op: "spawn_tagged", tag, writes }]);
    this.nextTag = tag + 1;
    this.unanswered.push({ tag, body, tick: this.queue(body, true) });
    return tag;
  }
  despawn(entity: number): number {
    return this.sendOps([{ op: "despawn", entity }]);
  }
  /** Hands an entity you own to another actor in the room. */
  transfer(entity: number, to: number): number {
    return this.sendOps([{ op: "transfer", entity, to }]);
  }
  /** Owner only: ownership policy for an entity (see OwnerPolicy). */
  setPolicy(entity: number, policy: OwnerPolicy): number {
    return this.sendOps([{ op: "set_policy", entity, policy }]);
  }
  /** Take an unowned entity or a dynamic one; refused = owner stays unchanged in the next delta. */
  requestOwnership(entity: number): number {
    return this.sendOps([{ op: "request", entity }]);
  }
  /** Drop ownership; the entity stays alive unowned. */
  release(entity: number): number {
    return this.sendOps([{ op: "release", entity }]);
  }
  writeEntity(
    entity: number,
    writes: Array<[ComponentDef, ComponentValue]>,
  ): number {
    return this.sendOps([{ op: "write", entity, writes }]);
  }
}
