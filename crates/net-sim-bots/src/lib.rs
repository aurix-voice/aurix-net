//! Deterministic bot kit: data-driven behavior trees and hierarchical state machines with an
//! integer blackboard, tick-counted timers (no wall clock), snapshot/restore/hash, and glue to
//! drive `net-sim-nav` crowd agents. Game-specific conditions and actions are integers resolved
//! by a `Brain` implementation, so trees/machines are plain data that can ship in a lockstep
//! room's WASM module or be authored in tools and serialized as JSON.
#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use net_fpmath::{Fx, FxVec2};
use net_sim_nav::{Crowd, NavMesh};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Status {
    Running,
    Success,
    Failure,
}

/// Integer-keyed state shared by a bot's tree/machine and the game; values are raw i64 so
/// positions (Q32.32), ids and counters all fit.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Blackboard {
    pub values: BTreeMap<u32, i64>,
}

impl Blackboard {
    pub fn get(&self, k: u32) -> i64 {
        self.values.get(&k).copied().unwrap_or(0)
    }
    pub fn set(&mut self, k: u32, v: i64) {
        self.values.insert(k, v);
    }
    pub fn get_fx(&self, k: u32) -> Fx {
        Fx(self.get(k))
    }
    pub fn set_fx(&mut self, k: u32, v: Fx) {
        self.set(k, v.0);
    }
    pub fn get_vec(&self, k: u32) -> FxVec2 {
        FxVec2::new(self.get_fx(k), self.get_fx(k + 1))
    }
    pub fn set_vec(&mut self, k: u32, v: FxVec2) {
        self.set_fx(k, v.x);
        self.set_fx(k + 1, v.y);
    }
    pub fn has(&self, k: u32) -> bool {
        self.values.contains_key(&k)
    }
    pub fn clear(&mut self, k: u32) {
        self.values.remove(&k);
    }
}

/// Game hooks: conditions and actions are referenced by id from trees and machines.
pub trait Brain {
    fn condition(&mut self, id: u32, bb: &Blackboard) -> bool;
    /// Called every tick while the action node is active; return `Running` to keep going.
    fn action(&mut self, id: u32, bb: &mut Blackboard) -> Status;
    /// Fired when an action node is interrupted while `Running` (higher-priority branch won).
    fn abort(&mut self, _id: u32, _bb: &mut Blackboard) {}
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Node {
    /// Runs children in order; fails on the first failure.
    Sequence(Vec<Node>),
    /// Runs children in order; succeeds on the first success.
    Selector(Vec<Node>),
    /// Runs all children each tick; succeeds when `need` succeed, fails when that becomes impossible.
    Parallel {
        need: u32,
        children: Vec<Node>,
    },
    /// Game condition; `Success`/`Failure` instantly.
    Condition(u32),
    /// Game action; may run across ticks.
    Action(u32),
    Inverter(Box<Node>),
    Succeeder(Box<Node>),
    /// Repeat `times` (0 = forever) while the child succeeds.
    Repeat {
        times: u32,
        child: Box<Node>,
    },
    /// Fails while on cooldown; after the child finishes, blocks for `ticks`.
    Cooldown {
        ticks: u32,
        child: Box<Node>,
    },
    /// Runs the child until it finishes or `ticks` elapse (then fails and aborts it).
    Timeout {
        ticks: u32,
        child: Box<Node>,
    },
    /// Succeeds after `ticks` of running.
    Wait(u32),
    /// Blackboard compare: value at `key` op `value` (op: 0 ==, 1 !=, 2 <, 3 <=, 4 >, 5 >=).
    Compare {
        key: u32,
        op: u8,
        value: i64,
    },
    /// Set `key` to `value`; always succeeds.
    Set {
        key: u32,
        value: i64,
    },
}

/// Per-node runtime state, indexed by preorder position so trees stay plain data.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
struct NodeState {
    /// Sequence/Selector cursor, Repeat count, Wait/Timeout/Cooldown elapsed.
    counter: u32,
    /// Cooldown remaining ticks.
    cooldown: u32,
    running: bool,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BehaviorTree {
    pub root: Node,
    states: Vec<NodeState>,
    pub tick: u32,
}

fn count(n: &Node) -> usize {
    1 + match n {
        Node::Sequence(c) | Node::Selector(c) => c.iter().map(count).sum(),
        Node::Parallel { children, .. } => children.iter().map(count).sum(),
        Node::Inverter(c) | Node::Succeeder(c) => count(c),
        Node::Repeat { child, .. } | Node::Cooldown { child, .. } | Node::Timeout { child, .. } => {
            count(child)
        }
        _ => 0,
    }
}

impl BehaviorTree {
    pub fn new(root: Node) -> BehaviorTree {
        let n = count(&root);
        BehaviorTree {
            root,
            states: vec![NodeState::default(); n],
            tick: 0,
        }
    }
    pub fn from_json(bytes: &[u8]) -> Option<BehaviorTree> {
        serde_json::from_slice::<Node>(bytes)
            .ok()
            .map(BehaviorTree::new)
    }
    pub fn root_json(&self) -> Vec<u8> {
        serde_json::to_vec(&self.root).unwrap_or_default()
    }
    /// Reset all runtime state (aborting running actions).
    pub fn reset<B: Brain>(&mut self, brain: &mut B, bb: &mut Blackboard) {
        let root = self.root.clone();
        self.abort(&root, 0, brain, bb);
        for s in &mut self.states {
            *s = NodeState::default();
        }
    }
    pub fn tick<B: Brain>(&mut self, brain: &mut B, bb: &mut Blackboard) -> Status {
        let root = self.root.clone();
        let (st, _) = self.run(&root, 0, brain, bb);
        for s in &mut self.states {
            if s.cooldown > 0 {
                s.cooldown -= 1;
            }
        }
        self.tick += 1;
        st
    }
    pub fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |v: u64| {
            for b in v.to_le_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        mix(self.tick as u64);
        for s in &self.states {
            mix(s.counter as u64 | (s.cooldown as u64) << 32 | (s.running as u64) << 63);
        }
        h
    }

    /// Returns (status, nodes consumed) so siblings can locate their state slots.
    fn run<B: Brain>(
        &mut self,
        node: &Node,
        idx: usize,
        brain: &mut B,
        bb: &mut Blackboard,
    ) -> (Status, usize) {
        let size = count(node);
        let st = match node {
            Node::Condition(id) => {
                if brain.condition(*id, bb) {
                    Status::Success
                } else {
                    Status::Failure
                }
            }
            Node::Action(id) => {
                let st = brain.action(*id, bb);
                self.states[idx].running = st == Status::Running;
                st
            }
            Node::Compare { key, op, value } => {
                let v = bb.get(*key);
                let ok = match op {
                    0 => v == *value,
                    1 => v != *value,
                    2 => v < *value,
                    3 => v <= *value,
                    4 => v > *value,
                    _ => v >= *value,
                };
                if ok {
                    Status::Success
                } else {
                    Status::Failure
                }
            }
            Node::Set { key, value } => {
                bb.set(*key, *value);
                Status::Success
            }
            Node::Wait(ticks) => {
                let s = &mut self.states[idx];
                s.counter += 1;
                if s.counter >= *ticks {
                    s.counter = 0;
                    s.running = false;
                    Status::Success
                } else {
                    s.running = true;
                    Status::Running
                }
            }
            Node::Sequence(children) | Node::Selector(children) => {
                let is_seq = matches!(node, Node::Sequence(_));
                let start = self.states[idx].counter as usize;
                let mut child_idx = idx + 1;
                let mut result = if is_seq {
                    Status::Success
                } else {
                    Status::Failure
                };
                let mut i = 0;
                // abort children before the cursor that may still be running (memory semantics
                // keep the cursor, so nothing before it runs; just advance indices)
                while i < children.len() {
                    let c = &children[i];
                    if i < start {
                        // guards before the cursor are re-evaluated every tick so a running
                        // branch reacts when its condition flips (reactive sequences/selectors)
                        if matches!(c, Node::Condition(_) | Node::Compare { .. }) {
                            let (st, _) = self.run(c, child_idx, brain, bb);
                            if (is_seq && st == Status::Failure)
                                || (!is_seq && st == Status::Success)
                            {
                                let mut rest = child_idx + count(c);
                                for later in &children[i + 1..] {
                                    self.abort(later, rest, brain, bb);
                                    rest += count(later);
                                }
                                self.states[idx].counter = 0;
                                self.states[idx].running = false;
                                return (st, size);
                            }
                        }
                        child_idx += count(c);
                        i += 1;
                        continue;
                    }
                    let (st, used) = self.run(c, child_idx, brain, bb);
                    child_idx += used;
                    match st {
                        Status::Running => {
                            self.states[idx].counter = i as u32;
                            self.states[idx].running = true;
                            // abort any later children that were left running by a prior tick
                            let mut rest = child_idx;
                            for later in &children[i + 1..] {
                                self.abort(later, rest, brain, bb);
                                rest += count(later);
                            }
                            return (Status::Running, size);
                        }
                        Status::Success if !is_seq => {
                            result = Status::Success;
                            break;
                        }
                        Status::Failure if is_seq => {
                            result = Status::Failure;
                            break;
                        }
                        _ => {}
                    }
                    i += 1;
                }
                self.states[idx].counter = 0;
                self.states[idx].running = false;
                result
            }
            Node::Parallel { need, children } => {
                let mut child_idx = idx + 1;
                let (mut ok, mut failed) = (0u32, 0u32);
                for c in children {
                    let (st, used) = self.run(c, child_idx, brain, bb);
                    child_idx += used;
                    match st {
                        Status::Success => ok += 1,
                        Status::Failure => failed += 1,
                        Status::Running => {}
                    }
                }
                let total = children.len() as u32;
                if ok >= *need {
                    self.abort_children(node, idx, brain, bb);
                    Status::Success
                } else if total - failed < *need {
                    self.abort_children(node, idx, brain, bb);
                    Status::Failure
                } else {
                    Status::Running
                }
            }
            Node::Inverter(child) => match self.run(child, idx + 1, brain, bb).0 {
                Status::Success => Status::Failure,
                Status::Failure => Status::Success,
                Status::Running => Status::Running,
            },
            Node::Succeeder(child) => match self.run(child, idx + 1, brain, bb).0 {
                Status::Running => Status::Running,
                _ => Status::Success,
            },
            Node::Repeat { times, child } => match self.run(child, idx + 1, brain, bb).0 {
                Status::Running => Status::Running,
                Status::Failure => {
                    self.states[idx].counter = 0;
                    Status::Failure
                }
                Status::Success => {
                    let s = &mut self.states[idx];
                    s.counter += 1;
                    if *times > 0 && s.counter >= *times {
                        s.counter = 0;
                        Status::Success
                    } else {
                        Status::Running
                    }
                }
            },
            Node::Cooldown { ticks, child } => {
                if self.states[idx].cooldown > 0 && !self.states[idx].running {
                    Status::Failure
                } else {
                    let st = self.run(child, idx + 1, brain, bb).0;
                    let s = &mut self.states[idx];
                    s.running = st == Status::Running;
                    if st != Status::Running {
                        s.cooldown = *ticks + 1;
                    }
                    st
                }
            }
            Node::Timeout { ticks, child } => {
                let st = self.run(child, idx + 1, brain, bb).0;
                let s = &mut self.states[idx];
                if st == Status::Running {
                    s.counter += 1;
                    if s.counter >= *ticks {
                        s.counter = 0;
                        self.abort(child, idx + 1, brain, bb);
                        Status::Failure
                    } else {
                        Status::Running
                    }
                } else {
                    s.counter = 0;
                    st
                }
            }
        };
        (st, size)
    }

    fn abort_children<B: Brain>(
        &mut self,
        node: &Node,
        idx: usize,
        brain: &mut B,
        bb: &mut Blackboard,
    ) {
        if let Node::Parallel { children, .. } = node {
            let mut child_idx = idx + 1;
            for c in children {
                self.abort(c, child_idx, brain, bb);
                child_idx += count(c);
            }
        }
    }

    /// Stop anything running under `node` (cursor reset, running actions notified).
    fn abort<B: Brain>(&mut self, node: &Node, idx: usize, brain: &mut B, bb: &mut Blackboard) {
        match node {
            Node::Action(id) => {
                if self.states[idx].running {
                    brain.abort(*id, bb);
                }
            }
            Node::Sequence(children) | Node::Selector(children) => {
                let mut child_idx = idx + 1;
                for c in children {
                    self.abort(c, child_idx, brain, bb);
                    child_idx += count(c);
                }
            }
            Node::Parallel { children, .. } => {
                let mut child_idx = idx + 1;
                for c in children {
                    self.abort(c, child_idx, brain, bb);
                    child_idx += count(c);
                }
            }
            Node::Inverter(c) | Node::Succeeder(c) => self.abort(c, idx + 1, brain, bb),
            Node::Repeat { child, .. }
            | Node::Cooldown { child, .. }
            | Node::Timeout { child, .. } => self.abort(child, idx + 1, brain, bb),
            _ => {}
        }
        let s = &mut self.states[idx];
        s.counter = 0;
        s.running = false;
    }
}

// ---- hierarchical state machine --------------------------------------------------------------

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Transition {
    /// Game condition id; `None` = unconditional (taken when the state's action finishes).
    pub condition: Option<u32>,
    pub to: u32,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct State {
    pub id: u32,
    /// Parent state (hierarchy): parent transitions are checked after the child's own.
    pub parent: Option<u32>,
    /// Initial child when this state is entered and has children.
    pub initial: Option<u32>,
    pub on_enter: Option<u32>,
    pub on_exit: Option<u32>,
    /// Action run every tick while active (leaf states).
    pub action: Option<u32>,
    pub transitions: Vec<Transition>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StateMachine {
    pub states: BTreeMap<u32, State>,
    pub current: u32,
    pub ticks_in_state: u32,
    pub tick: u32,
    last_action: Status,
}

impl StateMachine {
    pub fn new(states: Vec<State>, initial: u32) -> StateMachine {
        let states: BTreeMap<u32, State> = states.into_iter().map(|s| (s.id, s)).collect();
        StateMachine {
            states,
            current: initial,
            ticks_in_state: 0,
            tick: 0,
            last_action: Status::Running,
        }
    }
    pub fn from_json(bytes: &[u8]) -> Option<StateMachine> {
        serde_json::from_slice(bytes).ok()
    }
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
    /// Enter `initial` (running on_enter hooks down the hierarchy); call once before ticking.
    pub fn start<B: Brain>(&mut self, brain: &mut B, bb: &mut Blackboard) {
        let mut cur = self.current;
        self.enter_chain(cur, brain, bb);
        while let Some(init) = self.states.get(&cur).and_then(|s| s.initial) {
            if let Some(a) = self.states[&init].on_enter {
                brain.action(a, bb);
            }
            cur = init;
        }
        self.current = cur;
        self.ticks_in_state = 0;
    }
    fn enter_chain<B: Brain>(&mut self, id: u32, brain: &mut B, bb: &mut Blackboard) {
        let mut chain = vec![id];
        let mut p = self.states.get(&id).and_then(|s| s.parent);
        while let Some(pp) = p {
            chain.push(pp);
            p = self.states.get(&pp).and_then(|s| s.parent);
        }
        for s in chain.iter().rev() {
            if let Some(a) = self.states[s].on_enter {
                brain.action(a, bb);
            }
        }
    }
    fn ancestors(&self, id: u32) -> Vec<u32> {
        let mut out = vec![id];
        let mut p = self.states.get(&id).and_then(|s| s.parent);
        while let Some(pp) = p {
            out.push(pp);
            p = self.states.get(&pp).and_then(|s| s.parent);
        }
        out
    }
    /// Is `id` the current state or one of its ancestors?
    pub fn is_in(&self, id: u32) -> bool {
        self.ancestors(self.current).contains(&id)
    }
    pub fn tick<B: Brain>(&mut self, brain: &mut B, bb: &mut Blackboard) {
        // transitions: own state first, then ancestors; first matching wins
        let chain = self.ancestors(self.current);
        let mut target = None;
        'outer: for s in &chain {
            for t in &self.states[s].transitions {
                let fire = match t.condition {
                    Some(c) => brain.condition(c, bb),
                    None => self.last_action != Status::Running && *s == self.current,
                };
                if fire {
                    target = Some(t.to);
                    break 'outer;
                }
            }
        }
        if let Some(to) = target {
            self.transition(to, brain, bb);
        }
        if let Some(a) = self.states.get(&self.current).and_then(|s| s.action) {
            self.last_action = brain.action(a, bb);
        } else {
            self.last_action = Status::Success;
        }
        self.ticks_in_state += 1;
        self.tick += 1;
    }
    /// Exit up to the common ancestor, enter down to `to` (and its initial children).
    pub fn transition<B: Brain>(&mut self, to: u32, brain: &mut B, bb: &mut Blackboard) {
        let from_chain = self.ancestors(self.current);
        let to_chain = self.ancestors(to);
        for s in &from_chain {
            if to_chain.contains(s) {
                break;
            }
            if let Some(a) = self.states[s].on_exit {
                brain.action(a, bb);
            }
        }
        for s in to_chain.iter().rev() {
            if from_chain.contains(s) {
                continue;
            }
            if let Some(a) = self.states[s].on_enter {
                brain.action(a, bb);
            }
        }
        let mut cur = to;
        while let Some(init) = self.states.get(&cur).and_then(|s| s.initial) {
            if let Some(a) = self.states[&init].on_enter {
                brain.action(a, bb);
            }
            cur = init;
        }
        self.current = cur;
        self.ticks_in_state = 0;
        self.last_action = Status::Running;
    }
    pub fn hash(&self) -> u64 {
        (self.current as u64)
            ^ (self.ticks_in_state as u64) << 32
            ^ (self.tick as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
    }
}

// ---- navmesh glue -----------------------------------------------------------------------------

/// Blackboard keys the nav helpers read/write (games may use any other keys freely).
pub mod keys {
    /// Crowd agent id of this bot.
    pub const AGENT: u32 = 1;
    /// Current target point (x at TARGET, y at TARGET + 1).
    pub const TARGET: u32 = 2;
    /// 1 while the agent reports arrival.
    pub const ARRIVED: u32 = 4;
    /// Distance to target (Fx) refreshed by `sync_agent`.
    pub const DIST: u32 = 5;
    /// Agent position (x, y).
    pub const POS: u32 = 6;
}

/// Copy the agent's position/arrival into the blackboard; call before ticking brains.
pub fn sync_agent(crowd: &Crowd, bb: &mut Blackboard) {
    let id = bb.get(keys::AGENT) as u32;
    if let Some(a) = crowd.agents.get(&id) {
        bb.set_vec(keys::POS, a.pos);
        bb.set(keys::ARRIVED, a.arrived as i64);
        if bb.has(keys::TARGET) {
            bb.set_fx(keys::DIST, (bb.get_vec(keys::TARGET) - a.pos).length());
        }
    }
}

/// Steer the bot's agent to `target` (clamped onto the mesh); returns false when unreachable.
pub fn go_to(crowd: &mut Crowd, mesh: &NavMesh, bb: &mut Blackboard, target: FxVec2) -> bool {
    let id = bb.get(keys::AGENT) as u32;
    let Some((_, on_mesh)) = mesh.nearest(target) else {
        return false;
    };
    let Some(a) = crowd.agents.get_mut(&id) else {
        return false;
    };
    if mesh.find_path(a.pos, on_mesh, u32::MAX).is_none() {
        return false;
    }
    a.target = Some(on_mesh);
    a.field = None;
    a.arrived = false;
    bb.set_vec(keys::TARGET, on_mesh);
    bb.set(keys::ARRIVED, 0);
    true
}

/// Follow a shared flow field (e.g. "all zombies to the player").
pub fn follow_field(crowd: &mut Crowd, bb: &mut Blackboard, field: u32) {
    let id = bb.get(keys::AGENT) as u32;
    if let Some(a) = crowd.agents.get_mut(&id) {
        a.field = Some(field);
        a.target = None;
        a.arrived = false;
    }
    if let Some(f) = crowd.fields.get(field as usize) {
        bb.set_vec(keys::TARGET, f.goal);
    }
    bb.set(keys::ARRIVED, 0);
}

pub fn stop(crowd: &mut Crowd, bb: &mut Blackboard) {
    let id = bb.get(keys::AGENT) as u32;
    if let Some(a) = crowd.agents.get_mut(&id) {
        a.target = None;
        a.field = None;
        a.vel = FxVec2::ZERO;
    }
    bb.clear(keys::TARGET);
}

#[cfg(test)]
mod tests {
    use super::*;
    use net_sim_nav::{Agent, CrowdConfig};

    const C_SEE: u32 = 1;
    const A_PATROL: u32 = 10;
    const A_CHASE: u32 = 11;
    const A_LOG: u32 = 12;

    #[derive(Default)]
    struct Stub {
        see: bool,
        log: Vec<&'static str>,
        patrol_left: u32,
        aborted: u32,
    }
    impl Brain for Stub {
        fn condition(&mut self, id: u32, _bb: &Blackboard) -> bool {
            match id {
                C_SEE => self.see,
                _ => false,
            }
        }
        fn action(&mut self, id: u32, bb: &mut Blackboard) -> Status {
            match id {
                A_PATROL => {
                    self.log.push("patrol");
                    if self.patrol_left == 0 {
                        self.patrol_left = 3;
                        Status::Success
                    } else {
                        self.patrol_left -= 1;
                        Status::Running
                    }
                }
                A_CHASE => {
                    self.log.push("chase");
                    bb.set(100, bb.get(100) + 1);
                    Status::Running
                }
                A_LOG => {
                    self.log.push("log");
                    Status::Success
                }
                _ => Status::Failure,
            }
        }
        fn abort(&mut self, _id: u32, _bb: &mut Blackboard) {
            self.aborted += 1;
        }
    }

    fn guard_tree() -> Node {
        Node::Selector(vec![
            Node::Sequence(vec![Node::Condition(C_SEE), Node::Action(A_CHASE)]),
            Node::Sequence(vec![
                Node::Action(A_PATROL),
                Node::Wait(2),
                Node::Action(A_LOG),
            ]),
        ])
    }

    #[test]
    fn tree_prioritises_chase_and_aborts_patrol() {
        let mut t = BehaviorTree::new(guard_tree());
        let mut bb = Blackboard::default();
        let mut brain = Stub {
            patrol_left: 2,
            ..Default::default()
        };
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Running); // patrol running
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Running);
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Running); // patrol done -> wait 1/2
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Success); // wait 2/2 -> log -> sequence done
        assert!(brain.log.contains(&"log"), "{:?}", brain.log);
        brain.see = true;
        brain.patrol_left = 5;
        t.tick(&mut brain, &mut bb);
        t.tick(&mut brain, &mut bb);
        assert_eq!(bb.get(100), 2, "chase ran twice");
        assert_eq!(brain.log.last(), Some(&"chase"));
        // memory: patrol restarted from scratch when chase stops
        brain.see = false;
        t.tick(&mut brain, &mut bb);
        assert_eq!(brain.log.last(), Some(&"patrol"));
        let json = t.root_json();
        let t2 = BehaviorTree::from_json(&json).unwrap();
        assert_eq!(t2.root, t.root);
    }

    #[test]
    fn decorators_cooldown_timeout_repeat() {
        let mut bb = Blackboard::default();
        let mut brain = Stub::default();
        let mut t = BehaviorTree::new(Node::Sequence(vec![
            Node::Cooldown {
                ticks: 3,
                child: Box::new(Node::Action(A_LOG)),
            },
            Node::Set { key: 7, value: 1 },
        ]));
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Success);
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Failure);
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Failure);
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Failure);
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Success);
        let mut t = BehaviorTree::new(Node::Timeout {
            ticks: 3,
            child: Box::new(Node::Action(A_CHASE)),
        });
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Running);
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Running);
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Failure);
        assert_eq!(brain.aborted, 1);
        let mut t = BehaviorTree::new(Node::Repeat {
            times: 3,
            child: Box::new(Node::Action(A_LOG)),
        });
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Running);
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Running);
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Success);
        let mut t = BehaviorTree::new(Node::Parallel {
            need: 1,
            children: vec![Node::Action(A_CHASE), Node::Wait(2)],
        });
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Running);
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Success);
        assert_eq!(brain.aborted, 2, "chase aborted when the parallel finished");
        let mut t = BehaviorTree::new(Node::Compare {
            key: 100,
            op: 5,
            value: 1,
        });
        assert_eq!(t.tick(&mut brain, &mut bb), Status::Success);
    }

    #[test]
    fn hfsm_transitions_with_hierarchy_hooks() {
        // Alive { Idle -> Chase on see } ; any Alive -> Dead when key 50 == 1 (parent transition)
        #[derive(Default)]
        struct Fsm {
            see: bool,
            log: Vec<String>,
        }
        impl Brain for Fsm {
            fn condition(&mut self, id: u32, bb: &Blackboard) -> bool {
                match id {
                    1 => self.see,
                    2 => bb.get(50) == 1,
                    _ => false,
                }
            }
            fn action(&mut self, id: u32, _bb: &mut Blackboard) -> Status {
                self.log.push(format!("a{id}"));
                if id == 20 {
                    Status::Success
                } else {
                    Status::Running
                }
            }
        }
        let states = vec![
            State {
                id: 1,
                parent: None,
                initial: Some(2),
                on_enter: Some(100),
                on_exit: Some(101),
                action: None,
                transitions: vec![Transition {
                    condition: Some(2),
                    to: 4,
                }],
            },
            State {
                id: 2,
                parent: Some(1),
                initial: None,
                on_enter: None,
                on_exit: None,
                action: Some(20),
                transitions: vec![Transition {
                    condition: Some(1),
                    to: 3,
                }],
            },
            State {
                id: 3,
                parent: Some(1),
                initial: None,
                on_enter: Some(102),
                on_exit: None,
                action: Some(30),
                transitions: vec![Transition {
                    condition: None,
                    to: 2,
                }],
            },
            State {
                id: 4,
                parent: None,
                initial: None,
                on_enter: Some(103),
                on_exit: None,
                action: None,
                transitions: vec![],
            },
        ];
        let mut m = StateMachine::new(states.clone(), 1);
        let mut m2 = StateMachine::new(states, 1);
        let mut bb = Blackboard::default();
        let mut brain = Fsm::default();
        let mut brain2 = Fsm::default();
        m.start(&mut brain, &mut bb);
        m2.start(&mut brain2, &mut bb.clone());
        assert_eq!(m.current, 2);
        assert_eq!(brain.log, vec!["a100"]);
        m.tick(&mut brain, &mut bb);
        m2.tick(&mut brain2, &mut bb.clone());
        assert_eq!(brain.log.last().unwrap(), "a20");
        brain.see = true;
        brain2.see = true;
        m.tick(&mut brain, &mut bb);
        m2.tick(&mut brain2, &mut bb.clone());
        assert_eq!(m.current, 3);
        assert!(m.is_in(1));
        assert_eq!(&brain.log[brain.log.len() - 2..], &["a102", "a30"]);
        assert_eq!(m.hash(), m2.hash());
        bb.set(50, 1);
        m.tick(&mut brain, &mut bb);
        assert_eq!(m.current, 4);
        assert_eq!(
            &brain.log[brain.log.len() - 2..],
            &["a101", "a103"],
            "exit Alive then enter Dead"
        );
        let back = StateMachine::from_json(&m.to_json()).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn nav_glue_moves_a_bot_to_its_target_deterministically() {
        let blocked: Vec<(u32, u32)> = (0..5).map(|r| (4, r)).collect();
        let mesh = NavMesh::grid(FxVec2::ZERO, Fx::ONE, 8, 6, &blocked);
        let run = || {
            let mut crowd = Crowd::new(CrowdConfig::default());
            let id = crowd.add(
                &mesh,
                Agent::new(
                    FxVec2::new(Fx::HALF, Fx::HALF),
                    Fx::from_ratio(1, 4),
                    Fx::from_int(3),
                ),
            );
            let mut bb = Blackboard::default();
            bb.set(keys::AGENT, id as i64);
            assert!(go_to(
                &mut crowd,
                &mesh,
                &mut bb,
                FxVec2::new(Fx::from_ratio(15, 2), Fx::HALF)
            ));
            let mut arrived_at = None;
            for t in 0..900 {
                crowd.step(&mesh);
                sync_agent(&crowd, &mut bb);
                if bb.get(keys::ARRIVED) == 1 && arrived_at.is_none() {
                    arrived_at = Some(t);
                    stop(&mut crowd, &mut bb);
                }
            }
            (arrived_at, crowd.hash(), bb.get_vec(keys::POS))
        };
        let (a1, h1, p1) = run();
        let (a2, h2, _) = run();
        assert!(a1.is_some(), "bot never arrived, last pos {p1:?}");
        assert_eq!(a1, a2);
        assert_eq!(h1, h2);
    }
    /// Cross-SDK vectors: a scripted brain drives the guard tree and the HFSM; per-tick status,
    /// tree hash and blackboard are recorded. `BOTS_WRITE_VECTORS=1` rewrites the fixture that
    /// the TS and C# ports replay.
    #[test]
    fn vectors_match_the_fixture() {
        struct Scripted {
            t: u32,
            patrol_left: u32,
        }
        impl Brain for Scripted {
            fn condition(&mut self, id: u32, bb: &Blackboard) -> bool {
                match id {
                    C_SEE => (8..=12).contains(&self.t) || (20..=22).contains(&self.t),
                    2 => bb.get(50) == 1,
                    _ => false,
                }
            }
            fn action(&mut self, id: u32, bb: &mut Blackboard) -> Status {
                match id {
                    A_PATROL => {
                        if self.patrol_left == 0 {
                            self.patrol_left = 3;
                            Status::Success
                        } else {
                            self.patrol_left -= 1;
                            Status::Running
                        }
                    }
                    A_CHASE => {
                        bb.set(100, bb.get(100) + 1);
                        Status::Running
                    }
                    A_LOG => {
                        bb.set(101, bb.get(101) + 1);
                        Status::Success
                    }
                    20 => Status::Success,
                    30 => Status::Running,
                    100..=103 => {
                        bb.set(id, bb.get(id) + 1);
                        Status::Success
                    }
                    _ => Status::Failure,
                }
            }
            fn abort(&mut self, _id: u32, bb: &mut Blackboard) {
                bb.set(102, bb.get(102) + 1);
            }
        }
        let tree_root = Node::Selector(vec![
            Node::Sequence(vec![
                Node::Condition(C_SEE),
                Node::Timeout {
                    ticks: 4,
                    child: Box::new(Node::Action(A_CHASE)),
                },
            ]),
            Node::Sequence(vec![
                Node::Cooldown {
                    ticks: 2,
                    child: Box::new(Node::Action(A_PATROL)),
                },
                Node::Wait(2),
                Node::Repeat {
                    times: 2,
                    child: Box::new(Node::Action(A_LOG)),
                },
                Node::Compare {
                    key: 101,
                    op: 5,
                    value: 4,
                },
                Node::Set { key: 7, value: 1 },
            ]),
        ]);
        let mut tree = BehaviorTree::new(tree_root.clone());
        let mut bb = Blackboard::default();
        let mut brain = Scripted {
            t: 0,
            patrol_left: 2,
        };
        let mut ticks = Vec::new();
        for t in 0..40u32 {
            brain.t = t;
            let st = tree.tick(&mut brain, &mut bb);
            ticks.push(serde_json::json!({
                "status": format!("{st:?}"), "hash": format!("{:#x}", tree.hash()),
                "bb": bb.values.iter().map(|(k, v)| (k.to_string(), *v)).collect::<BTreeMap<_, _>>(),
            }));
        }
        let states = vec![
            State {
                id: 1,
                parent: None,
                initial: Some(2),
                on_enter: Some(100),
                on_exit: Some(101),
                action: None,
                transitions: vec![Transition {
                    condition: Some(2),
                    to: 4,
                }],
            },
            State {
                id: 2,
                parent: Some(1),
                initial: None,
                on_enter: None,
                on_exit: None,
                action: Some(20),
                transitions: vec![Transition {
                    condition: Some(C_SEE),
                    to: 3,
                }],
            },
            State {
                id: 3,
                parent: Some(1),
                initial: None,
                on_enter: Some(102),
                on_exit: None,
                action: Some(30),
                transitions: vec![
                    Transition {
                        condition: None,
                        to: 2,
                    },
                    Transition {
                        condition: Some(3),
                        to: 2,
                    },
                ],
            },
            State {
                id: 4,
                parent: None,
                initial: None,
                on_enter: Some(103),
                on_exit: None,
                action: None,
                transitions: vec![],
            },
        ];
        let mut fsm = StateMachine::new(states, 1);
        let mut fbb = Blackboard::default();
        let mut fbrain = Scripted {
            t: 0,
            patrol_left: 0,
        };
        fsm.start(&mut fbrain, &mut fbb);
        let mut fsm_ticks = Vec::new();
        for t in 0..30u32 {
            fbrain.t = t;
            if t == 25 {
                fbb.set(50, 1);
            }
            fsm.tick(&mut fbrain, &mut fbb);
            fsm_ticks.push(serde_json::json!({
                "current": fsm.current, "hash": format!("{:#x}", fsm.hash()),
                "bb": fbb.values.iter().map(|(k, v)| (k.to_string(), *v)).collect::<BTreeMap<_, _>>(),
            }));
        }
        let doc = serde_json::json!({
            "tree": tree_root, "tree_ticks": ticks,
            "fsm": StateMachine::new(serde_json::from_slice::<StateMachine>(&fsm.to_json()).unwrap().states.into_values().collect(), 1),
            "fsm_ticks": fsm_ticks,
            "script": {"see_ticks": "8..=12, 20..=22", "dead_at": 25, "patrol_left_start": 2},
        });
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/vectors.json");
        let text = serde_json::to_string_pretty(&doc).unwrap();
        if std::env::var("BOTS_WRITE_VECTORS").is_ok() {
            std::fs::write(path, &text).unwrap();
        }
        let on_disk = std::fs::read_to_string(path)
            .expect("fixtures/vectors.json (run with BOTS_WRITE_VECTORS=1)");
        assert_eq!(
            on_disk.trim(),
            text.trim(),
            "bot vectors drifted from the fixture"
        );
    }
}
