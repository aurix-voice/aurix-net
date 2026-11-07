// Client side of lockstep rooms (port of the managed core): confirmed input bundles in tick order,
// late-join catch-up (checkpoint + journal tail), hash reports, checkpoint uploads when the node asks
// the master, adaptive input delay from the node's margin reports, and state dumps after a desync.
// The game owns the deterministic simulation: step it in onTick, hash it, restore it in onRestore.
import { Channel } from "./packet.js";
import { SyncWorld } from "./sync.js";

export const LS_INPUT = 0x10, LS_BUNDLE = 0x20, LS_HASH = 0x21, LS_CHECKPOINT = 0x22, LS_CHECKPOINT_REQ = 0x23, LS_CATCHUP = 0x24, LS_INPUT_STATS = 0x25, LS_DUMP_REQ = 0x26, LS_DUMP = 0x27;

export type TickInputs = Map<number, Uint8Array>;

export class LockstepClient {
  /** Confirmed inputs for one tick, in tick order, every tick exactly once (empty map = nobody sent). */
  onTick?: (tick: number, inputs: TickInputs) => void;
  /** Catch-up start: restore the game state from this checkpoint (empty state at tick 0 = fresh start). */
  onRestore?: (tick: number, hash: bigint, state: Uint8Array) => void;
  /** The node asked us (the master) for the confirmed state at a tick <= this one: answer with sendCheckpoint. */
  onCheckpointRequested?: (tick: number) => void;
  /** The node flagged a desync and wants our state at this tick (from your rollback history): answer with sendDump, or ignore. */
  onDumpRequested?: (tick: number) => void;

  private pending = new Map<number, TickInputs>();
  private recentInputs: Uint8Array[] = [];
  private started = false; private next = 0; private lastInput = 0; private slackStreak = 0;
  /** Next tick to be delivered to onTick. */
  get confirmedTick(): number { return this.next; }
  /** Newest tick the node has bundled (+1). */
  serverTick = 0;
  /** Ticks ahead of the server clock inputs are stamped; tuned between min/max from margin reports when adaptive. */
  inputDelay = 2; inputDelayMin = 1; inputDelayMax = 8; adaptiveDelay = true;
  /** Inputs repeated per datagram (1..8). */
  inputRedundancy = 3;
  lastMargin = 0; lateInputs = 0; delivered = 0; catchups = 0; dropped = 0;

  constructor(private readonly send: (channel: Channel, payload: Uint8Array) => void) {}

  /** Feed every data-path payload from a lockstep room. Returns false when it was not a lockstep frame. */
  receive(p: Uint8Array): boolean {
    if (p.length === 0) return false;
    switch (p[0]) {
      case LS_CATCHUP: {
        if (p.length < 17) return true;
        const tick = u32(p, 1), hash = u64(p, 5), len = u32(p, 13);
        const state = p.slice(17, 17 + Math.min(len, p.length - 17));
        this.pending.clear(); this.next = tick; this.started = true; this.catchups++;
        this.onRestore?.(tick, hash, state);
        return true;
      }
      case LS_BUNDLE: {
        if (p.length < 2) return true;
        const n = p[1]; let o = 2;
        for (let i = 0; i < n && o + 2 <= p.length; i++) { const len = (p[o] << 8) | p[o + 1]; o += 2; if (o + len > p.length) break; this.decodeBundle(p, o, len); o += len; }
        this.drain();
        return true;
      }
      case LS_CHECKPOINT_REQ: if (p.length >= 5) this.onCheckpointRequested?.(u32(p, 1)); return true;
      case LS_DUMP_REQ: if (p.length >= 5) this.onDumpRequested?.(u32(p, 1)); return true;
      case LS_INPUT_STATS: if (p.length >= 7) this.onInputStats((p[5] << 24) >> 24, p[6]); return true;
      case LS_CHECKPOINT: case LS_HASH: case LS_INPUT: return true;
    }
    return false;
  }

  /** Raises the delay as soon as inputs arrive late; lowers it one tick after three consecutive reports with >= 2 ticks of slack. */
  onInputStats(margin: number, late: number): void {
    this.lastMargin = margin; this.lateInputs += late;
    if (!this.adaptiveDelay) return;
    if (margin < 0) { this.inputDelay = Math.min(this.inputDelayMax, this.inputDelay + Math.min(4, -margin)); this.slackStreak = 0; }
    else if (margin >= 2) { if (++this.slackStreak >= 3) { this.slackStreak = 0; if (this.inputDelay > this.inputDelayMin) this.inputDelay--; } }
    else this.slackStreak = 0;
  }

  private decodeBundle(b: Uint8Array, o: number, len: number): void {
    const end = o + len; if (len < 6 || b[o] !== LS_BUNDLE) return;
    const t = u32(b, o + 1); const k = b[o + 5]; o += 6;
    if (t + 1 > this.serverTick) this.serverTick = t + 1;
    if (!this.started) { this.started = true; this.next = t; }
    if (t < this.next || this.pending.has(t)) return;
    const inputs: TickInputs = new Map();
    for (let i = 0; i < k && o + 3 <= end; i++) { const a = (b[o] << 8) | b[o + 1]; const l = b[o + 2]; o += 3; if (o + l > end) break; inputs.set(a, b.slice(o, o + l)); o += l; }
    this.pending.set(t, inputs);
  }

  private drain(): void {
    for (;;) { const inputs = this.pending.get(this.next); if (!inputs) break; this.pending.delete(this.next); const t = this.next++; this.delivered++; this.onTick?.(t, inputs); }
    for (const t of [...this.pending.keys()].sort((a, b) => a - b)) { if (t <= this.next + 600) break; this.pending.delete(t); this.dropped++; }
  }

  /** Queues this tick's local input (sent with the previous ones for redundancy); returns the tick it was stamped with. */
  sendInput(input: Uint8Array): number {
    const tick = Math.max(this.serverTick + this.inputDelay, this.lastInput + 1); this.lastInput = tick;
    this.recentInputs.push(input); const keep = Math.max(1, Math.min(8, this.inputRedundancy)); while (this.recentInputs.length > keep) this.recentInputs.shift();
    this.send(Channel.Unreliable, SyncWorld.encodeInput(tick - (this.recentInputs.length - 1), this.recentInputs));
    return tick;
  }

  reportHash(tick: number, hash: bigint): void { const b = new Uint8Array(13); b[0] = LS_HASH; w32(b, 1, tick); w64(b, 5, hash); this.send(Channel.Unreliable, b); }

  /** Master only, in answer to onCheckpointRequested: confirmed state at `tick` (<= requested). */
  sendCheckpoint(tick: number, hash: bigint, state: Uint8Array): void { this.send(Channel.Reliable, frame(LS_CHECKPOINT, tick, hash, state)); }
  /** Answer to onDumpRequested: your state at `tick` (<= 60 KiB), stored by the node for 24 h next to the master's. */
  sendDump(tick: number, hash: bigint, state: Uint8Array): void { this.send(Channel.Reliable, frame(LS_DUMP, tick, hash, state)); }

  /** FNV-1a 64 over bytes: a simple, portable state hash (same as the managed core). */
  static fnv1a(data: Uint8Array): bigint { let h = 14695981039346656037n; for (const x of data) { h ^= BigInt(x); h = (h * 1099511628211n) & 0xffffffffffffffffn; } return h; }
}

function frame(kind: number, tick: number, hash: bigint, state: Uint8Array): Uint8Array { const b = new Uint8Array(13 + state.length); b[0] = kind; w32(b, 1, tick); w64(b, 5, hash); b.set(state, 13); return b; }
function u32(b: Uint8Array, o: number): number { return ((b[o] << 24) | (b[o + 1] << 16) | (b[o + 2] << 8) | b[o + 3]) >>> 0; }
function u64(b: Uint8Array, o: number): bigint { return (BigInt(u32(b, o)) << 32n) | BigInt(u32(b, o + 4)); }
function w32(b: Uint8Array, o: number, v: number): void { b[o] = v >>> 24; b[o + 1] = (v >>> 16) & 255; b[o + 2] = (v >>> 8) & 255; b[o + 3] = v & 255; }
function w64(b: Uint8Array, o: number, v: bigint): void { w32(b, o, Number(v >> 32n)); w32(b, o + 4, Number(v & 0xffffffffn)); }
