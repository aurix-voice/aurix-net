//! Lockstep with prediction and rollback. The game supplies a deterministic `step` over a
//! cloneable state; this crate keeps the input journal, predicts missing inputs with each
//! actor's last known one, rewinds when a late input contradicts a prediction, and hashes
//! every confirmed tick so peers (and the server) can detect desyncs. The same journal is
//! the replay format.

use net_common::types::{ActorId, Tick};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

pub trait Hashable {
    fn state_hash(&self) -> u64;
}

/// Everyone's inputs for one tick, in actor order. Missing actors mean "predicted".
pub type TickInputs = BTreeMap<ActorId, Vec<u8>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replay {
    pub seed: u64,
    pub schema_hash: String,
    pub tick_hz: u16,
    pub actors: Vec<ActorId>,
    /// Tick of `inputs[0]` (0 unless the journal was trimmed on a very long match).
    #[serde(default)]
    pub from_tick: u32,
    /// Confirmed inputs per tick from `from_tick`; a missing actor keeps its previous input.
    pub inputs: Vec<TickInputs>,
    /// Hash after every `hash_every` ticks, for verification.
    pub hash_every: u32,
    pub hashes: Vec<(u32, u64)>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RollbackError {
    #[error("tick {0} is older than the rollback window")]
    TooOld(u32),
    #[error("tick {0} is too far ahead")]
    TooFar(u32),
}

#[derive(Debug, Clone)]
pub struct RollbackConfig {
    /// How many ticks back a late input can still be applied.
    pub window: usize,
    /// How far ahead of the last confirmed tick prediction may run.
    pub max_prediction: u32,
    pub hash_every: u32,
}

impl Default for RollbackConfig {
    fn default() -> Self {
        Self {
            window: 32,
            max_prediction: 8,
            hash_every: 10,
        }
    }
}

/// `S` is the whole deterministic game state.
pub struct Rollback<S: Clone + Hashable> {
    pub cfg: RollbackConfig,
    actors: Vec<ActorId>,
    /// Inputs received, by tick (relative to `base_tick`).
    journal: VecDeque<TickInputs>,
    /// Which (tick, actor) pairs are confirmed (the rest were predicted).
    confirmed: VecDeque<Vec<bool>>,
    /// State before each tick in the window; `states[0]` is the state at `base_tick`.
    states: VecDeque<S>,
    base_tick: u32,
    /// Current (predicted) tick: states.len() - 1 + base_tick.
    last_input: BTreeMap<ActorId, Vec<u8>>,
    hashes: Vec<(u32, u64)>,
    pub rollbacks: u64,
    pub resimulated_ticks: u64,
}

impl<S: Clone + Hashable> Rollback<S> {
    pub fn new(cfg: RollbackConfig, actors: Vec<ActorId>, initial: S) -> Self {
        let mut actors = actors;
        actors.sort();
        actors.dedup();
        Self {
            cfg,
            actors,
            journal: VecDeque::new(),
            confirmed: VecDeque::new(),
            states: VecDeque::from([initial]),
            base_tick: 0,
            last_input: BTreeMap::new(),
            hashes: Vec::new(),
            rollbacks: 0,
            resimulated_ticks: 0,
        }
    }

    /// Starts from a checkpoint: `state` is the state entering `tick` (every earlier input
    /// applied), as uploaded by the master and relayed by the server to late joiners.
    pub fn from_checkpoint(
        cfg: RollbackConfig,
        actors: Vec<ActorId>,
        tick: Tick,
        state: S,
    ) -> Self {
        let mut session = Self::new(cfg, actors, state);
        session.base_tick = tick.0;
        session
    }

    /// Newest state whose every input is confirmed, with the tick it is entering. This is
    /// what a master uploads as a checkpoint.
    pub fn confirmed_state(&self) -> Option<(Tick, &S)> {
        let confirmed = self.confirmed_tick()?;
        let len = (confirmed.0 - self.base_tick) as usize + 1;
        self.states.get(len).map(|s| (Tick(confirmed.0 + 1), s))
    }

    pub fn actors(&self) -> &[ActorId] {
        &self.actors
    }

    pub fn current_tick(&self) -> Tick {
        Tick(self.base_tick + (self.states.len() as u32 - 1))
    }

    pub fn state(&self) -> &S {
        self.states.back().unwrap()
    }

    /// Highest tick for which every actor's input is confirmed (none = base_tick - 1).
    pub fn confirmed_tick(&self) -> Option<Tick> {
        let mut newest = None;
        for (i, flags) in self.confirmed.iter().enumerate() {
            if flags.iter().all(|x| *x) {
                newest = Some(Tick(self.base_tick + i as u32));
            } else {
                break;
            }
        }
        newest
    }

    fn idx(&self, tick: u32) -> Option<usize> {
        if tick < self.base_tick {
            None
        } else {
            Some((tick - self.base_tick) as usize)
        }
    }

    /// Records a confirmed input. Returns true if a rollback happened (the input differed
    /// from what was predicted for an already simulated tick).
    pub fn add_input<F>(
        &mut self,
        actor: ActorId,
        tick: Tick,
        data: Vec<u8>,
        step: &mut F,
    ) -> Result<bool, RollbackError>
    where
        F: FnMut(&mut S, &TickInputs, Tick),
    {
        let Some(ai) = self.actors.iter().position(|a| *a == actor) else {
            return Ok(false);
        };
        let index = self.idx(tick.0).ok_or(RollbackError::TooOld(tick.0))?;
        let cur = self.current_tick().0;
        if tick.0 > cur + self.cfg.max_prediction {
            return Err(RollbackError::TooFar(tick.0));
        }
        while self.journal.len() <= index {
            self.journal.push_back(TickInputs::new());
            self.confirmed.push_back(vec![false; self.actors.len()]);
        }
        if self.confirmed[index][ai] {
            return Ok(false); // duplicate
        }
        let predicted = self.journal[index].get(&actor).cloned();
        self.journal[index].insert(actor, data.clone());
        self.confirmed[index][ai] = true;
        // Simulated already and prediction was wrong -> rewind.
        let simulated = index + 1 < self.states.len();
        if simulated && predicted.as_deref() != Some(data.as_slice()) {
            self.rollbacks += 1;
            self.resimulate_from(index, step);
            return Ok(true);
        }
        Ok(false)
    }

    fn resimulate_from<F>(&mut self, from: usize, step: &mut F)
    where
        F: FnMut(&mut S, &TickInputs, Tick),
    {
        let target = self.states.len() - 1;
        self.states.truncate(from + 1);
        // Rebuild last_input from confirmed history before i.
        self.last_input.clear();
        for t in 0..from {
            for (actor, data) in &self.journal[t] {
                self.last_input.insert(*actor, data.clone());
            }
        }
        for t in from..target {
            self.fill_predictions(t);
            let mut state = self.states[t].clone();
            step(
                &mut state,
                &self.journal[t],
                Tick(self.base_tick + t as u32),
            );
            for (actor, data) in &self.journal[t] {
                self.last_input.insert(*actor, data.clone());
            }
            self.states.push_back(state);
            self.resimulated_ticks += 1;
        }
    }

    fn fill_predictions(&mut self, index: usize) {
        while self.journal.len() <= index {
            self.journal.push_back(TickInputs::new());
            self.confirmed.push_back(vec![false; self.actors.len()]);
        }
        for actor in &self.actors {
            if !self.journal[index].contains_key(actor) {
                let data = self.last_input.get(actor).cloned().unwrap_or_default();
                self.journal[index].insert(*actor, data);
            }
        }
    }

    /// Advances one tick, predicting any missing inputs. Fails when prediction would run too
    /// far ahead of confirmed inputs (the caller should wait instead).
    pub fn advance<F>(&mut self, step: &mut F) -> Result<Tick, RollbackError>
    where
        F: FnMut(&mut S, &TickInputs, Tick),
    {
        let last = self.states.len() - 1;
        let cur = self.base_tick + last as u32;
        let confirmed = self
            .confirmed_tick()
            .map(|t| t.0 + 1)
            .unwrap_or(self.base_tick);
        if cur >= confirmed + self.cfg.max_prediction {
            return Err(RollbackError::TooFar(cur));
        }
        self.fill_predictions(last);
        let mut state = self.states[last].clone();
        step(&mut state, &self.journal[last], Tick(cur));
        for (actor, data) in &self.journal[last] {
            self.last_input.insert(*actor, data.clone());
        }
        self.states.push_back(state);
        self.trim();
        let now = self.current_tick();
        if now.0.is_multiple_of(self.cfg.hash_every)
            && self.confirmed_tick().is_some_and(|c| c.0 + 1 >= now.0)
        {
            self.hashes.push((now.0, self.state().state_hash()));
        }
        Ok(now)
    }

    /// Drops confirmed history older than the window.
    fn trim(&mut self) {
        let confirmed = self
            .confirmed_tick()
            .map(|t| t.0 + 1)
            .unwrap_or(self.base_tick);
        while self.states.len() > self.cfg.window + 1 && self.base_tick < confirmed {
            self.states.pop_front();
            self.journal.pop_front();
            self.confirmed.pop_front();
            self.base_tick += 1;
        }
    }

    pub fn hashes(&self) -> &[(u32, u64)] {
        &self.hashes
    }

    pub fn take_hashes(&mut self) -> Vec<(u32, u64)> {
        std::mem::take(&mut self.hashes)
    }
}

/// Replays a journal from scratch and returns the final state and hash checkpoints.
pub fn replay<S, F>(initial: S, replay: &Replay, mut step: F) -> (S, Vec<(u32, u64)>)
where
    S: Clone + Hashable,
    F: FnMut(&mut S, &TickInputs, Tick),
{
    let mut state = initial;
    let mut last: TickInputs = TickInputs::new();
    let mut hashes = Vec::new();
    for (t, inputs) in replay.inputs.iter().enumerate() {
        for (actor, data) in inputs {
            last.insert(*actor, data.clone());
        }
        let mut full = TickInputs::new();
        for actor in &replay.actors {
            full.insert(*actor, last.get(actor).cloned().unwrap_or_default());
        }
        step(&mut state, &full, Tick(t as u32));
        let now = t as u32 + 1;
        if now.is_multiple_of(replay.hash_every) {
            hashes.push((now, state.state_hash()));
        }
    }
    (state, hashes)
}

/// Canonical input journal collector (server side): confirmed inputs per tick.
#[derive(Debug, Default, Clone)]
pub struct Journal {
    /// Tick of `ticks[0]`; everything before it was trimmed (or never known after a restore).
    base: u32,
    /// The inputs of a tick packed one after another: actor (u16), length (u32), bytes.
    ticks: std::collections::VecDeque<Vec<u8>>,
}

fn pack(inputs: &TickInputs) -> Vec<u8> {
    let mut packed = Vec::with_capacity(inputs.values().map(|data| 6 + data.len()).sum());
    for (actor, data) in inputs {
        pack_input(&mut packed, *actor, data);
    }
    packed
}

fn pack_input(packed: &mut Vec<u8>, actor: ActorId, data: &[u8]) {
    packed.extend_from_slice(&actor.0.to_be_bytes());
    packed.extend_from_slice(&(data.len() as u32).to_be_bytes());
    packed.extend_from_slice(data);
}

fn unpack(mut packed: &[u8]) -> impl Iterator<Item = (ActorId, &[u8])> {
    std::iter::from_fn(move || {
        let (head, rest) = packed.split_first_chunk::<6>()?;
        let actor = ActorId(u16::from_be_bytes([head[0], head[1]]));
        let len = u32::from_be_bytes([head[2], head[3], head[4], head[5]]) as usize;
        let (data, rest) = rest.split_at_checked(len)?;
        packed = rest;
        Some((actor, data))
    })
}

fn inputs_of(packed: &[u8]) -> TickInputs {
    unpack(packed)
        .map(|(actor, data)| (actor, data.to_vec()))
        .collect()
}

impl Journal {
    pub fn record(&mut self, tick: Tick, actor: ActorId, data: Vec<u8>) {
        if tick.0 < self.base {
            return;
        }
        let index = (tick.0 - self.base) as usize;
        while self.ticks.len() <= index {
            self.ticks.push_back(Vec::new());
        }
        let packed = &mut self.ticks[index];
        if unpack(packed).all(|(known, _)| known != actor) {
            pack_input(packed, actor, &data);
        }
    }

    /// Logical length: one past the last recorded tick (counts trimmed/unknown ticks too).
    pub fn len(&self) -> usize {
        self.base as usize + self.ticks.len()
    }

    /// Ticks actually held in memory.
    pub fn live_len(&self) -> usize {
        self.ticks.len()
    }

    pub fn base(&self) -> u32 {
        self.base
    }

    pub fn is_empty(&self) -> bool {
        self.ticks.is_empty()
    }

    /// Forgets ticks before `tick` (keeps memory bounded on long matches; the replay then
    /// starts at `from_tick`).
    pub fn trim_before(&mut self, tick: u32) {
        while self.base < tick && !self.ticks.is_empty() {
            self.ticks.pop_front();
            self.base += 1;
        }
    }

    /// Every actor that ever submitted an input.
    pub fn actors(&self) -> Vec<ActorId> {
        let actors: std::collections::BTreeSet<ActorId> = self
            .ticks
            .iter()
            .flat_map(|packed| unpack(packed).map(|(actor, _)| actor))
            .collect();
        actors.into_iter().collect()
    }

    pub fn tick(&self, at: Tick) -> Option<TickInputs> {
        if at.0 < self.base {
            return None;
        }
        let packed = self.ticks.get((at.0 - self.base) as usize)?;
        Some(inputs_of(packed))
    }

    /// Inputs from tick `from` to the end (for re-hosting from a checkpoint). Ticks before the
    /// journal's base are unknown and come back empty.
    pub fn tail(&self, from: u32) -> Vec<TickInputs> {
        let unknown = self.base.saturating_sub(from) as usize;
        let known = from.saturating_sub(self.base) as usize;
        let mut ticks = vec![TickInputs::new(); unknown];
        ticks.extend(
            self.ticks
                .iter()
                .skip(known)
                .map(|packed| inputs_of(packed)),
        );
        ticks
    }

    /// Rebuilds a journal whose first `from` ticks are unknown and the rest is `tail`.
    pub fn from_parts(from: u32, tail: Vec<TickInputs>) -> Self {
        Self {
            base: from,
            ticks: tail.iter().map(pack).collect(),
        }
    }

    pub fn into_replay(
        self,
        seed: u64,
        schema_hash: String,
        tick_hz: u16,
        actors: Vec<ActorId>,
        hash_every: u32,
    ) -> Replay {
        Replay {
            seed,
            schema_hash,
            tick_hz,
            actors,
            from_tick: self.base,
            inputs: self.tail(self.base),
            hash_every,
            hashes: Vec::new(),
        }
    }
}

/// Hash helper for plain byte-serialisable states.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let hash = blake3::hash(bytes);
    u64::from_le_bytes(hash.as_bytes()[..8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct World {
        pos: BTreeMap<ActorId, i64>,
    }
    impl Hashable for World {
        fn state_hash(&self) -> u64 {
            let mut bytes = Vec::new();
            for (actor, position) in &self.pos {
                bytes.extend_from_slice(&actor.0.to_le_bytes());
                bytes.extend_from_slice(&position.to_le_bytes());
            }
            hash_bytes(&bytes)
        }
    }
    fn step(world: &mut World, inputs: &TickInputs, _t: Tick) {
        for (actor, data) in inputs {
            let dx = data.first().copied().unwrap_or(0) as i8 as i64;
            *world.pos.entry(*actor).or_default() += dx;
        }
    }
    fn world() -> World {
        World {
            pos: BTreeMap::new(),
        }
    }
    const A: ActorId = ActorId(1);
    const B: ActorId = ActorId(2);

    #[test]
    fn predicts_then_rolls_back_on_late_input() {
        let mut rb = Rollback::new(RollbackConfig::default(), vec![A, B], world());
        // tick 0: both confirmed, A=+1, B=+1
        rb.add_input(A, Tick(0), vec![1], &mut step).unwrap();
        rb.add_input(B, Tick(0), vec![1], &mut step).unwrap();
        rb.advance(&mut step).unwrap();
        // ticks 1..3: only A arrives; B predicted as +1
        for t in 1..4 {
            rb.add_input(A, Tick(t), vec![1], &mut step).unwrap();
            rb.advance(&mut step).unwrap();
        }
        assert_eq!(rb.state().pos[&A], 4);
        assert_eq!(rb.state().pos[&B], 4); // predicted
        assert_eq!(rb.confirmed_tick(), Some(Tick(0)));
        // B's real inputs: tick1 = -1 (prediction wrong), tick2 = +1 (right), tick3 = +1
        assert!(rb.add_input(B, Tick(1), vec![255], &mut step).unwrap());
        assert_eq!(rb.state().pos[&B], 2); // 1 -1 +1 +1
        assert!(!rb.add_input(B, Tick(2), vec![1], &mut step).unwrap());
        assert!(!rb.add_input(B, Tick(3), vec![1], &mut step).unwrap());
        assert_eq!(rb.confirmed_tick(), Some(Tick(3)));
        assert_eq!(rb.rollbacks, 1);
        assert_eq!(rb.state().pos[&A], 4);
        // duplicate is ignored, too-old rejected after trim
        assert!(!rb.add_input(B, Tick(3), vec![9], &mut step).unwrap());
    }

    #[test]
    fn prediction_limit_and_window() {
        let cfg = RollbackConfig {
            window: 4,
            max_prediction: 3,
            hash_every: 2,
        };
        let mut rb = Rollback::new(cfg, vec![A], world());
        for _ in 0..3 {
            rb.advance(&mut step).unwrap();
        }
        assert_eq!(rb.advance(&mut step), Err(RollbackError::TooFar(3)));
        for t in 0..3 {
            rb.add_input(A, Tick(t), vec![2], &mut step).unwrap();
        }
        assert_eq!(rb.state().pos[&A], 6); // rolled back from predicted 0s
        for t in 3..20 {
            rb.add_input(A, Tick(t), vec![1], &mut step).unwrap();
            rb.advance(&mut step).unwrap();
        }
        assert_eq!(rb.current_tick(), Tick(20));
        assert_eq!(
            rb.add_input(A, Tick(2), vec![1], &mut step),
            Err(RollbackError::TooOld(2))
        );
        assert!(!rb.hashes().is_empty());
    }

    #[test]
    fn replay_reproduces_hashes() {
        let mut rb = Rollback::new(
            RollbackConfig {
                hash_every: 5,
                ..Default::default()
            },
            vec![A, B],
            world(),
        );
        let mut journal = Journal::default();
        for t in 0..40u32 {
            let da = vec![(t % 3) as u8];
            let db = vec![if t % 7 == 0 { 255 } else { 1 }];
            rb.add_input(A, Tick(t), da.clone(), &mut step).unwrap();
            journal.record(Tick(t), A, da);
            if t % 2 == 0 {
                // B's input sometimes arrives one tick late
                rb.add_input(B, Tick(t), db.clone(), &mut step).unwrap();
            }
            journal.record(Tick(t), B, db.clone());
            rb.advance(&mut step).unwrap();
            if t % 2 == 1 {
                // B's odd-tick input arrives one tick late (after we already predicted it)
                rb.add_input(B, Tick(t), db, &mut step).unwrap();
            }
        }
        let live = rb.state().clone();
        let rep = journal.into_replay(1, "h".into(), 60, vec![A, B], 5);
        let (replayed, hashes) = replay(world(), &rep, step);
        assert_eq!(replayed, live);
        assert_eq!(hashes.last().unwrap().1, live.state_hash());
        let live_hashes = rb.hashes();
        for (tick, hash) in live_hashes {
            if let Some((_, rh)) = hashes.iter().find(|(rt, _)| rt == tick) {
                assert_eq!(hash, rh, "tick {tick}");
            }
        }
        assert!(rb.rollbacks > 0);
    }
}
