// Deterministic bot kit, a port of `net-sim-bots`: data-driven behavior trees, hierarchical state
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
  get(key: number): bigint {
    return this.values.get(key) ?? 0n;
  }
  set(key: number, value: bigint | number) {
    this.values.set(key, BigInt(value));
  }
  has(key: number): boolean {
    return this.values.has(key);
  }
  clear(key: number) {
    this.values.delete(key);
  }
  /** Sorted snapshot (same order as the Rust BTreeMap). */
  entries(): [number, bigint][] {
    return [...this.values.entries()].sort((left, right) => left[0] - right[0]);
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

type NodeOf<K extends string> = Extract<BtNode, Record<K, unknown>>;

function kind(node: BtNode): string {
  return Object.keys(node)[0];
}
function children(node: BtNode): BtNode[] {
  const nodeKind = kind(node);
  switch (nodeKind) {
    case "Sequence":
      return (node as NodeOf<"Sequence">).Sequence;
    case "Selector":
      return (node as NodeOf<"Selector">).Selector;
    case "Parallel":
      return (node as NodeOf<"Parallel">).Parallel.children;
    case "Inverter":
      return [(node as NodeOf<"Inverter">).Inverter];
    case "Succeeder":
      return [(node as NodeOf<"Succeeder">).Succeeder];
    case "Repeat":
      return [(node as NodeOf<"Repeat">).Repeat.child];
    case "Cooldown":
      return [(node as NodeOf<"Cooldown">).Cooldown.child];
    case "Timeout":
      return [(node as NodeOf<"Timeout">).Timeout.child];
    default:
      return [];
  }
}
function count(node: BtNode): number {
  return 1 + children(node).reduce((id, child) => id + count(child), 0);
}

const FNV_OFFSET = 0xcbf29ce484222325n;
const FNV_PRIME = 0x100000001b3n;
const MASK64 = (1n << 64n) - 1n;
function fnvMix(hash: bigint, value: bigint): bigint {
  let x = BigInt.asUintN(64, value);
  for (let i = 0; i < 8; i++) {
    hash ^= x & 0xffn;
    hash = (hash * FNV_PRIME) & MASK64;
    x >>= 8n;
  }
  return hash;
}

export class BehaviorTree {
  private states: NodeState[];
  tick_ = 0;
  constructor(readonly root: BtNode) {
    this.states = Array.from({ length: count(root) }, () => ({
      counter: 0,
      cooldown: 0,
      running: false,
    }));
  }
  static fromJSON(text: string | BtNode): BehaviorTree {
    return new BehaviorTree(
      typeof text === "string" ? (JSON.parse(text) as BtNode) : text,
    );
  }
  get tick(): number {
    return this.tick_;
  }
  reset(brain: Brain, bb: Blackboard) {
    this.abort(this.root, 0, brain, bb);
    for (const state of this.states) {
      state.counter = 0;
      state.cooldown = 0;
      state.running = false;
    }
  }
  step(brain: Brain, bb: Blackboard): Status {
    const st = this.run(this.root, 0, brain, bb);
    for (const state of this.states) {
      if (state.cooldown > 0) {
        state.cooldown--;
      }
    }
    this.tick_ = (this.tick_ + 1) >>> 0;
    return st;
  }
  hash(): bigint {
    let digest = fnvMix(FNV_OFFSET, BigInt(this.tick_));
    for (const state of this.states) {
      const value =
        BigInt(state.counter) |
        (BigInt(state.cooldown) << 32n) |
        (state.running ? 1n << 63n : 0n);
      digest = fnvMix(digest, value);
    }
    return digest;
  }

  private run(node: BtNode, idx: number, brain: Brain, bb: Blackboard): Status {
    const nodeKind = kind(node);
    const state = this.states[idx];
    switch (nodeKind) {
      case "Condition":
        return brain.condition((node as NodeOf<"Condition">).Condition, bb)
          ? "Success"
          : "Failure";
      case "Action": {
        const st = brain.action((node as NodeOf<"Action">).Action, bb);
        state.running = st === "Running";
        return st;
      }
      case "Compare": {
        const { key, op, value } = (node as NodeOf<"Compare">).Compare;
        const current = bb.get(key);
        const x = BigInt(value);
        const ok =
          op === 0
            ? current === x
            : op === 1
              ? current !== x
              : op === 2
                ? current < x
                : op === 3
                  ? current <= x
                  : op === 4
                    ? current > x
                    : current >= x;
        return ok ? "Success" : "Failure";
      }
      case "Set": {
        const { key, value } = (node as NodeOf<"Set">).Set;
        bb.set(key, BigInt(value));
        return "Success";
      }
      case "Wait": {
        state.counter++;
        if (state.counter >= (node as NodeOf<"Wait">).Wait) {
          state.counter = 0;
          state.running = false;
          return "Success";
        }
        state.running = true;
        return "Running";
      }
      case "Sequence":
      case "Selector": {
        const isSeq = nodeKind === "Sequence";
        const kids = children(node);
        const start = state.counter;
        let childIdx = idx + 1;
        let result: Status = isSeq ? "Success" : "Failure";
        for (let i = 0; i < kids.length; i++) {
          const kid = kids[i];
          if (i < start) {
            const ck = kind(kid);
            if (ck === "Condition" || ck === "Compare") {
              const st = this.run(kid, childIdx, brain, bb);
              if ((isSeq && st === "Failure") || (!isSeq && st === "Success")) {
                let rest = childIdx + count(kid);
                for (const later of kids.slice(i + 1)) {
                  this.abort(later, rest, brain, bb);
                  rest += count(later);
                }
                state.counter = 0;
                state.running = false;
                return st;
              }
            }
            childIdx += count(kid);
            continue;
          }
          const st = this.run(kid, childIdx, brain, bb);
          childIdx += count(kid);
          if (st === "Running") {
            state.counter = i;
            state.running = true;
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
        state.counter = 0;
        state.running = false;
        return result;
      }
      case "Parallel": {
        const { need } = (node as NodeOf<"Parallel">).Parallel;
        const kids = children(node);
        let childIdx = idx + 1;
        let ok = 0;
        let failed = 0;
        for (const kid of kids) {
          const st = this.run(kid, childIdx, brain, bb);
          childIdx += count(kid);
          if (st === "Success") {
            ok++;
          } else if (st === "Failure") {
            failed++;
          }
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
        const st = this.run(
          (node as NodeOf<"Inverter">).Inverter,
          idx + 1,
          brain,
          bb,
        );
        return st === "Success"
          ? "Failure"
          : st === "Failure"
            ? "Success"
            : "Running";
      }
      case "Succeeder": {
        const st = this.run(
          (node as NodeOf<"Succeeder">).Succeeder,
          idx + 1,
          brain,
          bb,
        );
        return st === "Running" ? "Running" : "Success";
      }
      case "Repeat": {
        const { times, child } = (node as NodeOf<"Repeat">).Repeat;
        const st = this.run(child, idx + 1, brain, bb);
        if (st === "Running") {
          return "Running";
        }
        if (st === "Failure") {
          state.counter = 0;
          return "Failure";
        }
        state.counter++;
        if (times > 0 && state.counter >= times) {
          state.counter = 0;
          return "Success";
        }
        return "Running";
      }
      case "Cooldown": {
        const { ticks, child } = (node as NodeOf<"Cooldown">).Cooldown;
        if (state.cooldown > 0 && !state.running) {
          return "Failure";
        }
        const st = this.run(child, idx + 1, brain, bb);
        state.running = st === "Running";
        if (st !== "Running") {
          state.cooldown = ticks + 1;
        }
        return st;
      }
      case "Timeout": {
        const { ticks, child } = (node as NodeOf<"Timeout">).Timeout;
        const st = this.run(child, idx + 1, brain, bb);
        if (st === "Running") {
          state.counter++;
          if (state.counter >= ticks) {
            state.counter = 0;
            this.abort(child, idx + 1, brain, bb);
            return "Failure";
          }
          return "Running";
        }
        state.counter = 0;
        return st;
      }
    }
    throw new Error(`unknown node ${nodeKind}`);
  }

  private abortChildren(
    node: BtNode,
    idx: number,
    brain: Brain,
    bb: Blackboard,
  ) {
    let childIdx = idx + 1;
    for (const child of children(node)) {
      this.abort(child, childIdx, brain, bb);
      childIdx += count(child);
    }
  }

  private abort(node: BtNode, idx: number, brain: Brain, bb: Blackboard) {
    const nodeKind = kind(node);
    if (nodeKind === "Action") {
      if (this.states[idx].running) {
        brain.abort?.((node as NodeOf<"Action">).Action, bb);
      }
    } else {
      let childIdx = idx + 1;
      for (const child of children(node)) {
        this.abort(child, childIdx, brain, bb);
        childIdx += count(child);
      }
    }
    const state = this.states[idx];
    state.counter = 0;
    state.running = false;
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

/** The Rust `StateMachine` JSON: `states` keyed by id, plus the running position. */
export interface StateMachineJson {
  states: Record<string, State>;
  current: number;
  ticks_in_state?: number;
  tick?: number;
  last_action?: Status;
}

export class StateMachine {
  readonly states = new Map<number, State>();
  current: number;
  ticksInState = 0;
  tick = 0;
  private lastAction: Status = "Running";
  constructor(states: State[], initial: number) {
    for (const state of states) {
      this.states.set(state.id, state);
    }
    this.current = initial;
  }
  /** Accepts the Rust `StateMachine` JSON (`states` object keyed by id). */
  static fromJSON(text: string | StateMachineJson): StateMachine {
    const json =
      typeof text === "string" ? (JSON.parse(text) as StateMachineJson) : text;
    const machine = new StateMachine(Object.values(json.states), json.current);
    machine.ticksInState = json.ticks_in_state ?? 0;
    machine.tick = json.tick ?? 0;
    machine.lastAction = json.last_action ?? "Running";
    return machine;
  }
  start(brain: Brain, bb: Blackboard) {
    let cur = this.current;
    for (const id of this.ancestors(cur).reverse()) {
      const action = this.states.get(id)?.on_enter;
      if (action != null) {
        brain.action(action, bb);
      }
    }
    for (;;) {
      const init = this.states.get(cur)?.initial;
      if (init == null) {
        break;
      }
      const action = this.states.get(init)?.on_enter;
      if (action != null) {
        brain.action(action, bb);
      }
      cur = init;
    }
    this.current = cur;
    this.ticksInState = 0;
  }
  ancestors(id: number): number[] {
    const out = [id];
    let parent = this.states.get(id)?.parent;
    while (parent != null) {
      out.push(parent);
      parent = this.states.get(parent)?.parent;
    }
    return out;
  }
  isIn(id: number): boolean {
    return this.ancestors(this.current).includes(id);
  }
  step(brain: Brain, bb: Blackboard) {
    const chain = this.ancestors(this.current);
    let target: number | null = null;
    outer: for (const id of chain) {
      for (const transition of this.states.get(id)!.transitions) {
        const fire =
          transition.condition != null
            ? brain.condition(transition.condition, bb)
            : this.lastAction !== "Running" && id === this.current;
        if (fire) {
          target = transition.to;
          break outer;
        }
      }
    }
    if (target != null) {
      this.transition(target, brain, bb);
    }
    const action = this.states.get(this.current)?.action;
    this.lastAction = action != null ? brain.action(action, bb) : "Success";
    this.ticksInState = (this.ticksInState + 1) >>> 0;
    this.tick = (this.tick + 1) >>> 0;
  }
  transition(to: number, brain: Brain, bb: Blackboard) {
    const from = this.ancestors(this.current);
    const toChain = this.ancestors(to);
    for (const id of from) {
      if (toChain.includes(id)) {
        break;
      }
      const action = this.states.get(id)?.on_exit;
      if (action != null) {
        brain.action(action, bb);
      }
    }
    for (const id of [...toChain].reverse()) {
      if (from.includes(id)) {
        continue;
      }
      const action = this.states.get(id)?.on_enter;
      if (action != null) {
        brain.action(action, bb);
      }
    }
    let cur = to;
    for (;;) {
      const init = this.states.get(cur)?.initial;
      if (init == null) {
        break;
      }
      const action = this.states.get(init)?.on_enter;
      if (action != null) {
        brain.action(action, bb);
      }
      cur = init;
    }
    this.current = cur;
    this.ticksInState = 0;
    this.lastAction = "Running";
  }
  /** Same formula as Rust: current ^ ticks_in_state << 32 ^ tick * golden. */
  hash(): bigint {
    const golden = 0x9e3779b97f4a7c15n;
    return BigInt.asUintN(
      64,
      BigInt(this.current) ^
        (BigInt(this.ticksInState) << 32n) ^
        ((BigInt(this.tick) * golden) & MASK64),
    );
  }
}
