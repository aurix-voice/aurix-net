// Deterministic bot kit — port of `net-sim-bots`: data-driven behavior trees, hierarchical state
// machines and an integer blackboard. Node JSON is serde's externally tagged form, so trees
// authored for Rust/WASM run unchanged here; hashes match the Rust crate bit for bit.

export type Status = "Running" | "Success" | "Failure";

export type BtNode =
  | { Sequence: BtNode[] }
  | { Selector: BtNode[] }
  | { Parallel: { need: number; children: BtNode[] } }
  | { Condition: number }
  | { Action: number }
  | { Inverter: BtNode }
  | { Succeeder: BtNode }
  | { Repeat: { times: number; child: BtNode } }
  | { Cooldown: { ticks: number; child: BtNode } }
  | { Timeout: { ticks: number; child: BtNode } }
  | { Wait: number }
  | { Compare: { key: number; op: number; value: number | bigint } }
  | { Set: { key: number; value: number | bigint } };

export class Blackboard {
  readonly values = new Map<number, bigint>();
  get(k: number): bigint {
    return this.values.get(k) ?? 0n;
  }
  set(k: number, v: bigint | number) {
    this.values.set(k, BigInt(v));
  }
  has(k: number): boolean {
    return this.values.has(k);
  }
  clear(k: number) {
    this.values.delete(k);
  }
  /** Sorted snapshot (same order as the Rust BTreeMap). */
  entries(): [number, bigint][] {
    return [...this.values.entries()].sort((a, b) => a[0] - b[0]);
  }
}

export interface Brain {
  condition(id: number, bb: Blackboard): boolean;
  action(id: number, bb: Blackboard): Status;
  abort?(id: number, bb: Blackboard): void;
}

interface NodeState {
  counter: number;
  cooldown: number;
  running: boolean;
}

function kind(n: BtNode): string {
  return Object.keys(n)[0];
}
function children(n: BtNode): BtNode[] {
  const k = kind(n);
  if (k === "Sequence" || k === "Selector") return (n as any)[k];
  if (k === "Parallel") return (n as any).Parallel.children;
  if (k === "Inverter" || k === "Succeeder") return [(n as any)[k]];
  if (k === "Repeat" || k === "Cooldown" || k === "Timeout") return [(n as any)[k].child];
  return [];
}
function count(n: BtNode): number {
  return 1 + children(n).reduce((s, c) => s + count(c), 0);
}

const FNV_OFFSET = 0xcbf29ce484222325n;
const FNV_PRIME = 0x100000001b3n;
const MASK64 = (1n << 64n) - 1n;
function fnvMix(h: bigint, v: bigint): bigint {
  let x = BigInt.asUintN(64, v);
  for (let i = 0; i < 8; i++) {
    h ^= x & 0xffn;
    h = (h * FNV_PRIME) & MASK64;
    x >>= 8n;
  }
  return h;
}

export class BehaviorTree {
  private states: NodeState[];
  tick_ = 0;
  constructor(readonly root: BtNode) {
    this.states = Array.from({ length: count(root) }, () => ({ counter: 0, cooldown: 0, running: false }));
  }
  static fromJSON(text: string | BtNode): BehaviorTree {
    return new BehaviorTree(typeof text === "string" ? (JSON.parse(text) as BtNode) : text);
  }
  get tick(): number {
    return this.tick_;
  }
  reset(brain: Brain, bb: Blackboard) {
    this.abort(this.root, 0, brain, bb);
    for (const s of this.states) {
      s.counter = 0;
      s.cooldown = 0;
      s.running = false;
    }
  }
  step(brain: Brain, bb: Blackboard): Status {
    const st = this.run(this.root, 0, brain, bb);
    for (const s of this.states) if (s.cooldown > 0) s.cooldown--;
    this.tick_ = (this.tick_ + 1) >>> 0;
    return st;
  }
  hash(): bigint {
    let h = fnvMix(FNV_OFFSET, BigInt(this.tick_));
    for (const s of this.states) {
      const v = BigInt(s.counter) | (BigInt(s.cooldown) << 32n) | (s.running ? 1n << 63n : 0n);
      h = fnvMix(h, v);
    }
    return h;
  }

  private run(node: BtNode, idx: number, brain: Brain, bb: Blackboard): Status {
    const k = kind(node);
    const s = this.states[idx];
    switch (k) {
      case "Condition":
        return brain.condition((node as any).Condition, bb) ? "Success" : "Failure";
      case "Action": {
        const st = brain.action((node as any).Action, bb);
        s.running = st === "Running";
        return st;
      }
      case "Compare": {
        const { key, op, value } = (node as any).Compare;
        const v = bb.get(key);
        const x = BigInt(value);
        const ok = op === 0 ? v === x : op === 1 ? v !== x : op === 2 ? v < x : op === 3 ? v <= x : op === 4 ? v > x : v >= x;
        return ok ? "Success" : "Failure";
      }
      case "Set": {
        const { key, value } = (node as any).Set;
        bb.set(key, BigInt(value));
        return "Success";
      }
      case "Wait": {
        s.counter++;
        if (s.counter >= (node as any).Wait) {
          s.counter = 0;
          s.running = false;
          return "Success";
        }
        s.running = true;
        return "Running";
      }
      case "Sequence":
      case "Selector": {
        const isSeq = k === "Sequence";
        const kids = children(node);
        const start = s.counter;
        let childIdx = idx + 1;
        let result: Status = isSeq ? "Success" : "Failure";
        for (let i = 0; i < kids.length; i++) {
          const c = kids[i];
          if (i < start) {
            const ck = kind(c);
            if (ck === "Condition" || ck === "Compare") {
              const st = this.run(c, childIdx, brain, bb);
              if ((isSeq && st === "Failure") || (!isSeq && st === "Success")) {
                let rest = childIdx + count(c);
                for (const later of kids.slice(i + 1)) {
                  this.abort(later, rest, brain, bb);
                  rest += count(later);
                }
                s.counter = 0;
                s.running = false;
                return st;
              }
            }
            childIdx += count(c);
            continue;
          }
          const st = this.run(c, childIdx, brain, bb);
          childIdx += count(c);
          if (st === "Running") {
            s.counter = i;
            s.running = true;
            let rest = childIdx;
            for (const later of kids.slice(i + 1)) {
              this.abort(later, rest, brain, bb);
              rest += count(later);
            }
            return "Running";
          }
          if (!isSeq && st === "Success") {
            result = "Success";
            break;
          }
          if (isSeq && st === "Failure") {
            result = "Failure";
            break;
          }
        }
        s.counter = 0;
        s.running = false;
        return result;
      }
      case "Parallel": {
        const { need } = (node as any).Parallel;
        const kids = children(node);
        let childIdx = idx + 1;
        let ok = 0;
        let failed = 0;
        for (const c of kids) {
          const st = this.run(c, childIdx, brain, bb);
          childIdx += count(c);
          if (st === "Success") ok++;
          else if (st === "Failure") failed++;
        }
        if (ok >= need) {
          this.abortChildren(node, idx, brain, bb);
          return "Success";
        }
        if (kids.length - failed < need) {
          this.abortChildren(node, idx, brain, bb);
          return "Failure";
        }
        return "Running";
      }
      case "Inverter": {
        const st = this.run((node as any).Inverter, idx + 1, brain, bb);
        return st === "Success" ? "Failure" : st === "Failure" ? "Success" : "Running";
      }
      case "Succeeder": {
        const st = this.run((node as any).Succeeder, idx + 1, brain, bb);
        return st === "Running" ? "Running" : "Success";
      }
      case "Repeat": {
        const { times, child } = (node as any).Repeat;
        const st = this.run(child, idx + 1, brain, bb);
        if (st === "Running") return "Running";
        if (st === "Failure") {
          s.counter = 0;
          return "Failure";
        }
        s.counter++;
        if (times > 0 && s.counter >= times) {
          s.counter = 0;
          return "Success";
        }
        return "Running";
      }
      case "Cooldown": {
        const { ticks, child } = (node as any).Cooldown;
        if (s.cooldown > 0 && !s.running) return "Failure";
        const st = this.run(child, idx + 1, brain, bb);
        s.running = st === "Running";
        if (st !== "Running") s.cooldown = ticks + 1;
        return st;
      }
      case "Timeout": {
        const { ticks, child } = (node as any).Timeout;
        const st = this.run(child, idx + 1, brain, bb);
        if (st === "Running") {
          s.counter++;
          if (s.counter >= ticks) {
            s.counter = 0;
            this.abort(child, idx + 1, brain, bb);
            return "Failure";
          }
          return "Running";
        }
        s.counter = 0;
        return st;
      }
    }
    throw new Error(`unknown node ${k}`);
  }

  private abortChildren(node: BtNode, idx: number, brain: Brain, bb: Blackboard) {
    let childIdx = idx + 1;
    for (const c of children(node)) {
      this.abort(c, childIdx, brain, bb);
      childIdx += count(c);
    }
  }

  private abort(node: BtNode, idx: number, brain: Brain, bb: Blackboard) {
    const k = kind(node);
    if (k === "Action") {
      if (this.states[idx].running) brain.abort?.((node as any).Action, bb);
    } else {
      let childIdx = idx + 1;
      for (const c of children(node)) {
        this.abort(c, childIdx, brain, bb);
        childIdx += count(c);
      }
    }
    const s = this.states[idx];
    s.counter = 0;
    s.running = false;
  }
}

export interface Transition {
  condition: number | null;
  to: number;
}
export interface State {
  id: number;
  parent: number | null;
  initial: number | null;
  on_enter: number | null;
  on_exit: number | null;
  action: number | null;
  transitions: Transition[];
}

export class StateMachine {
  readonly states = new Map<number, State>();
  current: number;
  ticksInState = 0;
  tick = 0;
  private lastAction: Status = "Running";
  constructor(states: State[], initial: number) {
    for (const s of states) this.states.set(s.id, s);
    this.current = initial;
  }
  /** Accepts the Rust `StateMachine` JSON (`states` object keyed by id). */
  static fromJSON(text: string | any): StateMachine {
    const d = typeof text === "string" ? JSON.parse(text) : text;
    const m = new StateMachine(Object.values(d.states) as State[], d.current);
    m.ticksInState = d.ticks_in_state ?? 0;
    m.tick = d.tick ?? 0;
    m.lastAction = d.last_action ?? "Running";
    return m;
  }
  start(brain: Brain, bb: Blackboard) {
    let cur = this.current;
    for (const s of this.ancestors(cur).reverse()) {
      const a = this.states.get(s)?.on_enter;
      if (a != null) brain.action(a, bb);
    }
    for (;;) {
      const init = this.states.get(cur)?.initial;
      if (init == null) break;
      const a = this.states.get(init)?.on_enter;
      if (a != null) brain.action(a, bb);
      cur = init;
    }
    this.current = cur;
    this.ticksInState = 0;
  }
  ancestors(id: number): number[] {
    const out = [id];
    let p = this.states.get(id)?.parent;
    while (p != null) {
      out.push(p);
      p = this.states.get(p)?.parent;
    }
    return out;
  }
  isIn(id: number): boolean {
    return this.ancestors(this.current).includes(id);
  }
  step(brain: Brain, bb: Blackboard) {
    const chain = this.ancestors(this.current);
    let target: number | null = null;
    outer: for (const s of chain) {
      for (const t of this.states.get(s)!.transitions) {
        const fire = t.condition != null ? brain.condition(t.condition, bb) : this.lastAction !== "Running" && s === this.current;
        if (fire) {
          target = t.to;
          break outer;
        }
      }
    }
    if (target != null) this.transition(target, brain, bb);
    const a = this.states.get(this.current)?.action;
    this.lastAction = a != null ? brain.action(a, bb) : "Success";
    this.ticksInState = (this.ticksInState + 1) >>> 0;
    this.tick = (this.tick + 1) >>> 0;
  }
  transition(to: number, brain: Brain, bb: Blackboard) {
    const from = this.ancestors(this.current);
    const toChain = this.ancestors(to);
    for (const s of from) {
      if (toChain.includes(s)) break;
      const a = this.states.get(s)?.on_exit;
      if (a != null) brain.action(a, bb);
    }
    for (const s of [...toChain].reverse()) {
      if (from.includes(s)) continue;
      const a = this.states.get(s)?.on_enter;
      if (a != null) brain.action(a, bb);
    }
    let cur = to;
    for (;;) {
      const init = this.states.get(cur)?.initial;
      if (init == null) break;
      const a = this.states.get(init)?.on_enter;
      if (a != null) brain.action(a, bb);
      cur = init;
    }
    this.current = cur;
    this.ticksInState = 0;
    this.lastAction = "Running";
  }
  /** Same formula as Rust: current ^ ticks_in_state << 32 ^ tick * golden. */
  hash(): bigint {
    const golden = 0x9e3779b97f4a7c15n;
    return BigInt.asUintN(64, BigInt(this.current) ^ (BigInt(this.ticksInState) << 32n) ^ ((BigInt(this.tick) * golden) & MASK64));
  }
}
