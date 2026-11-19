// Client side of lockstep rooms (port of the managed core): confirmed input bundles in tick order,
// late-join catch-up (checkpoint + journal tail), hash reports, checkpoint uploads when the node asks
// the master, adaptive input delay from the node's margin reports, and state dumps after a desync.
// The game owns the deterministic simulation: step it in onTick, hash it, restore it in onRestore.
import { MarginControl } from "./margin.js";
import { Channel } from "./packet.js";
import { SyncWorld } from "./sync.js";

export const LS_INPUT = 0x10;
export const LS_BUNDLE = 0x20;
export const LS_HASH = 0x21;
export const LS_CHECKPOINT = 0x22;
export const LS_CHECKPOINT_REQ = 0x23;
export const LS_CATCHUP = 0x24;
export const LS_RESTORE = 0x2a;
export const LS_INPUT_STATS = 0x25;
export const LS_DUMP_REQ = 0x26;
export const LS_DUMP = 0x27;
const LS_COMMAND = 0x28;
const LS_ACK = 0x29;
/** Pseudo-actor whose per-tick input carries the commands. */
export const COMMAND_ACTOR = 0xffff;
/** Longest command a tick can carry. */
export const MAX_COMMAND = 251;

export type TickInputs = Map<number, Uint8Array>;

/** A command of a tick and the actor who sent it. */
export interface Command {
  actor: number;
  data: Uint8Array;
}

export class LockstepClient {
  /** Confirmed inputs for one tick, in tick order, every tick exactly once (empty map = nobody sent), and the commands of that tick in the order every client gets them. */
  onTick?: (tick: number, inputs: TickInputs, commands: Command[]) => void;
  /** Reliable command delivered to everyone at the same tick (sender actor, bytes); fires before onTick of that tick. */
  onCommand?: (tick: number, actor: number, data: Uint8Array) => void;
  /** Catch-up start: restore the game state from this checkpoint (empty state at tick 0 = fresh start). */
  onRestore?: (tick: number, hash: bigint, state: Uint8Array) => void;
  /** The node asked us (the master) for the confirmed state at a tick <= this one: answer with sendCheckpoint. */
  onCheckpointRequested?: (tick: number) => void;
  /** The node flagged a desync and wants our state at this tick (from your rollback history): answer with sendDump, or ignore. */
  onDumpRequested?: (tick: number) => void;

  private pending = new Map<number, TickInputs>();
  private recentInputs: Uint8Array[] = [];
  private started = false;
  private anchored = false;
  private next = 0;
  private lastInput = 0;
  private readonly margin = new MarginControl();
  /** Next tick to be delivered to onTick. */
  get confirmedTick(): number {
    return this.next;
  }
  /** Newest tick the node has bundled (+1). */
  serverTick = 0;
  /** Ticks ahead of the server clock inputs are stamped; tuned between min/max from margin reports when adaptive. */
  inputDelay = 2;
  inputDelayMin = 1;
  inputDelayMax = 32;
  adaptiveDelay = true;
  /** Inputs repeated per datagram (1..8). */
  inputRedundancy = 3;
  lastMargin = 0;
  lateInputs = 0;
  delivered = 0;
  catchups = 0;
  dropped = 0;

  constructor(
    private readonly send: (channel: Channel, payload: Uint8Array) => void,
  ) {}

  /** Feed every data-path payload from a lockstep room. Returns false when it was not a lockstep frame. */
  receive(packet: Uint8Array): boolean {
    if (packet.length === 0) {
      return false;
    }
    switch (packet[0]) {
      case LS_CATCHUP:
      case LS_RESTORE: {
        if (packet.length < 17) {
          return true;
        }
        const tick = u32(packet, 1);
        const hash = u64(packet, 5);
        const len = u32(packet, 13);
        const state = packet.slice(17, 17 + Math.min(len, packet.length - 17));
        // the catch-up of a join is reliable and can come after bundles that already took the client
        // past it; a restore always rewinds
        if (packet[0] === LS_CATCHUP && this.anchored && tick < this.next) {
          return true;
        }
        this.pending.clear();
        this.next = tick;
        this.started = true;
        this.anchored = true;
        this.catchups++;
        this.onRestore?.(tick, hash, state);
        return true;
      }
      case LS_BUNDLE: {
        if (packet.length < 2) {
          return true;
        }
        const count = packet[1];
        let offset = 2;
        for (let i = 0; i < count && offset + 2 <= packet.length; i++) {
          const len = (packet[offset] << 8) | packet[offset + 1];
          offset += 2;
          if (offset + len > packet.length) {
            break;
          }
          this.decodeBundle(packet, offset, len);
          offset += len;
        }
        this.drain();
        this.sendAck();
        return true;
      }
      case LS_CHECKPOINT_REQ:
        if (packet.length >= 5) {
          this.onCheckpointRequested?.(u32(packet, 1));
        }
        return true;
      case LS_DUMP_REQ:
        if (packet.length >= 5) {
          this.onDumpRequested?.(u32(packet, 1));
        }
        return true;
      case LS_INPUT_STATS:
        if (packet.length >= 7) {
          this.onInputStats(u32(packet, 1), (packet[5] << 24) >> 24, packet[6]);
        }
        return true;
      case LS_CHECKPOINT:
      case LS_HASH:
      case LS_INPUT:
        return true;
    }
    return false;
  }

  /** Keeps one or two ticks of slack: a late report raises the delay by the lateness plus one tick;
   *  three consecutive reports with more than two ticks to spare lower it by the excess. A late report
   *  that still covers inputs stamped before the last raise does not raise again. */
  onInputStats(tick: number, margin: number, late: number): void {
    this.lastMargin = margin;
    this.lateInputs += late;
    if (!this.adaptiveDelay) {
      return;
    }
    const change = this.margin.adjust(tick, margin, this.lastInput);
    this.inputDelay = Math.min(
      this.inputDelayMax,
      Math.max(this.inputDelayMin, this.inputDelay + change),
    );
  }

  /** Tells the room which tick this client needs next: the room then sends every bundle from that
   *  tick on instead of the last three, so a run of lost packets leaves no gap. */
  private sendAck(): void {
    const bytes = new Uint8Array(5);
    bytes[0] = LS_ACK;
    new DataView(bytes.buffer).setUint32(1, this.anchored ? this.next : 0);
    this.send(Channel.Unreliable, bytes);
  }

  private decodeBundle(bytes: Uint8Array, offset: number, len: number): void {
    const end = offset + len;
    if (len < 6 || bytes[offset] !== LS_BUNDLE) {
      return;
    }
    const tick = u32(bytes, offset + 1);
    const count = bytes[offset + 5];
    offset += 6;
    if (tick + 1 > this.serverTick) {
      this.serverTick = tick + 1;
    }
    this.started = true;
    // ticks are delivered from tick 0 or from a catch-up, never from the middle
    if (!this.anchored) {
      if (tick !== 0) {
        return;
      }
      this.anchored = true;
    }
    if (tick < this.next || this.pending.has(tick)) {
      return;
    }
    const inputs: TickInputs = new Map();
    for (let i = 0; i < count && offset + 3 <= end; i++) {
      const actor = (bytes[offset] << 8) | bytes[offset + 1];
      const length = bytes[offset + 2];
      offset += 3;
      if (offset + length > end) {
        break;
      }
      inputs.set(actor, bytes.slice(offset, offset + length));
      offset += length;
    }
    this.pending.set(tick, inputs);
  }

  private drain(): void {
    for (;;) {
      const inputs = this.pending.get(this.next);
      if (!inputs) {
        break;
      }
      this.pending.delete(this.next);
      const tick = this.next++;
      this.delivered++;
      const entry = inputs.get(COMMAND_ACTOR);
      inputs.delete(COMMAND_ACTOR);
      const commands = entry ? readCommands(entry) : [];
      for (const command of commands) {
        this.onCommand?.(tick, command.actor, command.data);
      }
      this.onTick?.(tick, inputs, commands);
    }
    for (const tick of [...this.pending.keys()].sort(
      (left, right) => left - right,
    )) {
      if (tick <= this.next + 600) {
        break;
      }
      this.pending.delete(tick);
      this.dropped++;
    }
  }

  /** Queues this tick's local input (sent with the previous ones for redundancy); returns the tick it was stamped with. */
  /** Reliable command outside the input stream (buy, surrender, pause): stamped onto the next tick by the server, journaled, replayed, seen by the referee. 1..MAX_COMMAND bytes. */
  sendCommand(data: Uint8Array): void {
    if (data.length === 0 || data.length > MAX_COMMAND) {
      throw new Error(`command: 1..${MAX_COMMAND} bytes`);
    }
    const bytes = new Uint8Array(1 + data.length);
    bytes[0] = LS_COMMAND;
    bytes.set(data, 1);
    this.send(Channel.Reliable, bytes);
  }
  /** Sends this tick's input and returns the tick it is stamped with. The room takes one input per tick:
   *  called more often than that it sends nothing and returns -1, so sample the input again next time.
   *  It does the same before the first frame from the room, while the room's tick is unknown. */
  sendInput(input: Uint8Array): number {
    if (!this.started) {
      return -1;
    }
    const tick = Math.max(
      this.serverTick + this.inputDelay,
      this.lastInput + 1,
    );
    if (tick > this.serverTick + this.inputDelay + 1) {
      return -1;
    }
    // a frame carries inputs for consecutive ticks, so earlier ones are resent only without a gap
    if (tick !== this.lastInput + 1) {
      this.recentInputs = [];
    }
    this.lastInput = tick;
    this.recentInputs.push(input);
    const keep = Math.max(1, Math.min(8, this.inputRedundancy));
    while (this.recentInputs.length > keep) {
      this.recentInputs.shift();
    }
    this.send(
      Channel.Unreliable,
      SyncWorld.encodeInput(
        tick - (this.recentInputs.length - 1),
        this.recentInputs,
      ),
    );
    return tick;
  }

  reportHash(tick: number, hash: bigint): void {
    const bytes = new Uint8Array(13);
    bytes[0] = LS_HASH;
    w32(bytes, 1, tick);
    w64(bytes, 5, hash);
    this.send(Channel.Unreliable, bytes);
  }

  /** Master only, in answer to onCheckpointRequested: confirmed state at `tick` (<= requested). */
  sendCheckpoint(tick: number, hash: bigint, state: Uint8Array): void {
    this.send(Channel.Reliable, frame(LS_CHECKPOINT, tick, hash, state));
  }
  /** Answer to onDumpRequested: your state at `tick` (<= 60 KiB), stored by the node for 24 h next to the master's. */
  sendDump(tick: number, hash: bigint, state: Uint8Array): void {
    this.send(Channel.Reliable, frame(LS_DUMP, tick, hash, state));
  }

  /** FNV-1a 64 over bytes: a simple, portable state hash (same as the managed core). */
  static fnv1a(data: Uint8Array): bigint {
    let hash = 14695981039346656037n;
    for (const x of data) {
      hash ^= BigInt(x);
      hash = (hash * 1099511628211n) & 0xffffffffffffffffn;
    }
    return hash;
  }
}

function frame(
  kind: number,
  tick: number,
  hash: bigint,
  state: Uint8Array,
): Uint8Array {
  const bytes = new Uint8Array(13 + state.length);
  bytes[0] = kind;
  w32(bytes, 1, tick);
  w64(bytes, 5, hash);
  bytes.set(state, 13);
  return bytes;
}
function u32(bytes: Uint8Array, offset: number): number {
  return (
    ((bytes[offset] << 24) |
      (bytes[offset + 1] << 16) |
      (bytes[offset + 2] << 8) |
      bytes[offset + 3]) >>>
    0
  );
}
function u64(bytes: Uint8Array, offset: number): bigint {
  return (BigInt(u32(bytes, offset)) << 32n) | BigInt(u32(bytes, offset + 4));
}
function readCommands(entry: Uint8Array): Command[] {
  const commands: Command[] = [];
  let offset = 0;
  while (offset + 4 <= entry.length) {
    const actor = (entry[offset] << 8) | entry[offset + 1];
    const len = (entry[offset + 2] << 8) | entry[offset + 3];
    offset += 4;
    if (offset + len > entry.length) {
      break;
    }
    commands.push({ actor, data: entry.slice(offset, offset + len) });
    offset += len;
  }
  return commands;
}
function w32(bytes: Uint8Array, offset: number, value: number): void {
  bytes[offset] = value >>> 24;
  bytes[offset + 1] = (value >>> 16) & 255;
  bytes[offset + 2] = (value >>> 8) & 255;
  bytes[offset + 3] = value & 255;
}
function w64(bytes: Uint8Array, offset: number, value: bigint): void {
  w32(bytes, offset, Number(value >> 32n));
  w32(bytes, offset + 4, Number(value & 0xffffffffn));
}

/** The game's deterministic simulation for LockstepRollback: step one tick, snapshot, restore. */
export interface Simulation<S = unknown> {
  /** One tick: the commands in their order, then the players' inputs. A predicted tick has no commands. */
  step(tick: number, inputs: TickInputs, commands: Command[]): void;
  snapshot(): S;
  restore(state: S): void;
}

/**
 * Predict/rollback on top of LockstepClient (same core as the Rust/C# SDKs): the game runs ahead of
 * the confirmed tick with predicted remote inputs (each actor's last confirmed input, repeated) and
 * its own inputs as sent, keeps a snapshot per predicted tick, and when a confirmed tick differs from
 * the prediction restores the snapshot, re-steps it and re-predicts the ticks after it. Commands are
 * never predicted: a predicted tick has none, and a confirmed tick that carries some is stepped again
 * with them.
 */
export class LockstepRollback<S = unknown> {
  private confirmed: number;
  private head: number;
  private history = new Map<number, S>();
  private predicted = new Map<number, TickInputs>();
  private localInputs = new Map<number, Uint8Array>();
  private lastLocal?: Uint8Array;
  private lastKnown: TickInputs = new Map();
  rollbacks = 0;
  resimulatedTicks = 0;
  predictedTicks = 0;

  /** `sim` must be at tick `start` (fresh game, or restored from a catch-up checkpoint). */
  constructor(
    private readonly sim: Simulation<S>,
    private readonly localActor: number,
    start = 0,
    public maxPrediction = 8,
  ) {
    this.confirmed = this.head = start;
    this.history.set(start, sim.snapshot());
  }
  /** First tick not yet confirmed by the node. */
  get confirmedTick(): number {
    return this.confirmed;
  }
  /** Next tick the simulation will run; head - confirmedTick ticks are predicted. */
  get headTick(): number {
    return this.head;
  }
  /** State after the last confirmed tick: hash *this* for reportHash, never the predicted head. */
  confirmedSnapshot(): S {
    return this.history.get(this.confirmed) as S;
  }

  /** After the game restored a catch-up checkpoint at `tick`: forget every prediction. */
  reset(tick: number): void {
    this.history.clear();
    this.history.set(tick, this.sim.snapshot());
    this.predicted.clear();
    this.localInputs.clear();
    this.confirmed = this.head = tick;
  }
  /** Remember the input sent stamped with `tick` (what LockstepClient.sendInput returned) so our own prediction uses exactly it. */
  localInput(tick: number, input: Uint8Array): void {
    this.lastLocal = input;
    if (tick >= 0) {
      this.localInputs.set(tick, input);
    }
  }

  /** Runs the simulation ahead to `target` (exclusive), bounded by maxPrediction. */
  predict(target: number): void {
    target = Math.min(target, this.confirmed + Math.max(1, this.maxPrediction));
    while (this.head < target) {
      const tick = this.head;
      const inputs = this.guess(tick);
      this.sim.step(tick, inputs, []);
      this.predicted.set(tick, inputs);
      this.head = tick + 1;
      this.history.set(this.head, this.sim.snapshot());
      this.predictedTicks++;
    }
  }
  /** Feeds a confirmed tick with its commands (LockstepClient.onTick). Returns true when a rollback happened. */
  confirm(tick: number, inputs: TickInputs, commands: Command[]): boolean {
    if (tick !== this.confirmed) {
      return false;
    }
    for (const [actor, input] of inputs) {
      this.lastKnown.set(actor, input);
    }
    this.localInputs.delete(tick);
    let rolled = false;
    if (tick >= this.head) {
      this.sim.step(tick, inputs, commands);
      this.head = tick + 1;
      this.history.set(this.head, this.sim.snapshot());
    } else if (
      commands.length > 0 ||
      !sameInputs(this.predicted.get(tick), inputs)
    ) {
      rolled = true;
      this.rollbacks++;
      this.sim.restore(this.history.get(tick) as S);
      this.sim.step(tick, inputs, commands);
      this.history.set(tick + 1, this.sim.snapshot());
      const oldHead = this.head;
      this.head = tick + 1;
      for (const predictedTick of [...this.predicted.keys()]) {
        if (predictedTick > tick) {
          this.predicted.delete(predictedTick);
        }
      }
      this.resimulatedTicks += oldHead - this.head;
      this.predict(oldHead);
    }
    this.confirmed = tick + 1;
    this.predicted.delete(tick);
    this.history.delete(tick);
    return rolled;
  }

  private guess(tick: number): TickInputs {
    const inputs: TickInputs = new Map(this.lastKnown);
    const mine = this.localInputs.get(tick) ?? this.lastLocal;
    if (mine) {
      inputs.set(this.localActor, mine);
    } else {
      inputs.delete(this.localActor);
    }
    return inputs;
  }
}

function sameInputs(left: TickInputs | undefined, right: TickInputs): boolean {
  if (!left || left.size !== right.size) {
    return false;
  }
  for (const [key, value] of left) {
    const other = right.get(key);
    if (!other || other.length !== value.length) {
      return false;
    }
    for (let i = 0; i < value.length; i++) {
      if (value[i] !== other[i]) {
        return false;
      }
    }
  }
  return true;
}

/** What ReplayRing.window hands back: restore `snapshot`, then step from `start`, tick by tick, with `inputs[i]` and `commands[i]`. */
export interface ReplayWindow<S> {
  start: number;
  snapshot: S;
  inputs: TickInputs[];
  commands: Command[][];
}

/**
 * Instant replay (kill cam) buffer: the inputs and commands of at least the last `keep` confirmed ticks, back to a
 * snapshot of the confirmed state taken every `every` ticks, so any recent moment can be re-simulated
 * locally. Feed it from the confirmed path (after LockstepRollback.confirm, with confirmedSnapshot()).
 */
export class ReplayRing<S = unknown> {
  private first = 0;
  private inputs: TickInputs[] = [];
  private commands: Command[][] = [];
  private snaps = new Map<number, S>();
  constructor(
    private readonly keep = 300,
    private readonly every = 10,
  ) {
    this.keep = Math.max(1, keep);
    this.every = Math.max(1, every);
  }

  /** Records confirmed `tick` with its inputs, its commands and the confirmed state *before* it (taken lazily on snapshot boundaries). A gap resets the ring. */
  record(
    tick: number,
    inputs: TickInputs,
    commands: Command[],
    stateBefore: () => S,
  ): void {
    if (this.inputs.length === 0 || this.first + this.inputs.length !== tick) {
      this.inputs = [];
      this.commands = [];
      this.snaps.clear();
      this.first = tick;
    }
    if (this.snaps.size === 0 || tick % this.every === 0) {
      this.snaps.set(tick, stateBefore());
    }
    this.inputs.push(inputs);
    this.commands.push(commands);
    // the oldest tick kept is one with a snapshot, so every tick kept can be stepped from one
    const wanted = tick + 1 - this.keep;
    let anchor = -1;
    for (const key of this.snaps.keys()) {
      if (key <= wanted && key > anchor) {
        anchor = key;
      }
    }
    if (anchor >= 0) {
      for (const key of [...this.snaps.keys()]) {
        if (key < anchor) {
          this.snaps.delete(key);
        }
      }
      this.inputs.splice(0, anchor - this.first);
      this.commands.splice(0, anchor - this.first);
      this.first = anchor;
    }
  }
  /** Oldest tick that can still be replayed. */
  get oldest(): number | undefined {
    if (this.inputs.length === 0) {
      return undefined;
    }
    let lowest: number | undefined;
    for (const key of this.snaps.keys()) {
      if (lowest === undefined || key < lowest) {
        lowest = key;
      }
    }
    return lowest;
  }
  /** Everything needed to re-simulate from the newest snapshot at or before `from` (clamped to what is kept) through the last recorded tick. */
  window(from: number): ReplayWindow<S> | undefined {
    let start = -1;
    for (const key of this.snaps.keys()) {
      if (key <= from && key > start) {
        start = key;
      }
    }
    if (start < 0) {
      const oldest = this.oldest;
      if (oldest === undefined) {
        return undefined;
      }
      start = oldest;
    }
    return {
      start,
      snapshot: this.snaps.get(start) as S,
      inputs: this.inputs.slice(Math.max(0, start - this.first)),
      commands: this.commands.slice(Math.max(0, start - this.first)),
    };
  }
}
