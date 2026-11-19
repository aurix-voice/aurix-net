//! Predict/rollback on top of lockstep: the game runs ahead of the confirmed tick with predicted
//! remote inputs (each actor's last confirmed input, repeated) and its own inputs as sent, keeps a
//! snapshot per predicted tick, and when the node confirms a tick whose inputs differ from the
//! prediction it restores the snapshot, re-steps that tick and re-predicts the ticks after it.
//! Local actions therefore render immediately; the simulation stays deterministic because only
//! confirmed inputs ever reach the confirmed state. Commands are never predicted: a predicted tick
//! has none, and a confirmed tick that carries some is stepped again with them. Same core as the
//! C#/Web `LockstepRollback`.

use std::collections::BTreeMap;

use bytes::Bytes;

use crate::lockstep::{take_commands, Command, TickInputs};

/// The game's deterministic simulation: step by one tick, snapshot and restore.
pub trait Simulation {
    type Snapshot;
    /// One tick: the commands in their order, then the players' inputs. A predicted tick has no
    /// commands.
    fn step(&mut self, tick: u32, inputs: &TickInputs, commands: &[Command]);
    fn snapshot(&self) -> Self::Snapshot;
    fn restore(&mut self, snapshot: &Self::Snapshot);
}

pub struct Rollback<S: Simulation> {
    local_actor: u16,
    /// Ticks the simulation may run ahead of the confirmed tick.
    pub max_prediction: u32,
    /// First tick not yet confirmed by the node.
    confirmed: u32,
    /// Next tick to simulate (confirmed..head are predicted).
    head: u32,
    /// Snapshot of the state *before* tick t, for every t in confirmed..=head.
    history: BTreeMap<u32, S::Snapshot>,
    predicted: BTreeMap<u32, TickInputs>,
    local_inputs: BTreeMap<u32, Bytes>,
    /// Newest input we sent: the guess for local ticks not yet stamped.
    last_local: Option<Bytes>,
    last_known: TickInputs,
    pub rollbacks: u64,
    pub resimulated_ticks: u64,
    pub predicted_ticks: u64,
}

impl<S: Simulation> Rollback<S> {
    /// `sim` must be at tick `start` (fresh game, or restored from a catch-up checkpoint).
    pub fn new(sim: &S, local_actor: u16, start: u32, max_prediction: u32) -> Self {
        let mut history = BTreeMap::new();
        history.insert(start, sim.snapshot());
        Self {
            local_actor,
            max_prediction: max_prediction.max(1),
            confirmed: start,
            head: start,
            history,
            predicted: BTreeMap::new(),
            local_inputs: BTreeMap::new(),
            last_local: None,
            last_known: TickInputs::new(),
            rollbacks: 0,
            resimulated_ticks: 0,
            predicted_ticks: 0,
        }
    }

    /// After the game restored a catch-up checkpoint at `tick`: forget every prediction.
    pub fn reset(&mut self, sim: &S, tick: u32) {
        self.history.clear();
        self.history.insert(tick, sim.snapshot());
        self.predicted.clear();
        self.local_inputs.clear();
        self.confirmed = tick;
        self.head = tick;
    }

    pub fn confirmed_tick(&self) -> u32 {
        self.confirmed
    }

    /// State after the last confirmed tick: hash *this* for `hash_frame`, never the predicted head.
    pub fn confirmed_snapshot(&self) -> &S::Snapshot {
        &self.history[&self.confirmed]
    }

    /// Next tick the simulation will run; `head() - confirmed_tick()` ticks are predicted.
    pub fn head(&self) -> u32 {
        self.head
    }

    /// Remember the input we sent stamped with `tick` (the value `LockstepClient::send_input`
    /// returned) so our own prediction uses exactly it.
    pub fn local_input(&mut self, tick: u32, input: Bytes) {
        self.last_local = Some(input.clone());
        self.local_inputs.insert(tick, input);
    }

    /// Runs the simulation ahead to `target` (exclusive), bounded by `max_prediction`.
    pub fn predict(&mut self, sim: &mut S, target: u32) {
        let target = target.min(self.confirmed + self.max_prediction);
        while self.head < target {
            let tick = self.head;
            let inputs = self.guess(tick);
            sim.step(tick, &inputs, &[]);
            self.predicted.insert(tick, inputs);
            self.head = tick + 1;
            self.history.insert(self.head, sim.snapshot());
            self.predicted_ticks += 1;
        }
    }

    /// Feeds a confirmed tick as `LsEvent::Tick` delivers it, its commands among the inputs.
    /// Returns true when a rollback happened.
    pub fn confirm(&mut self, sim: &mut S, tick: u32, inputs: &TickInputs) -> bool {
        if tick != self.confirmed {
            return false;
        }
        let mut inputs = inputs.clone();
        let commands = take_commands(&mut inputs);
        for (actor, input) in &inputs {
            self.last_known.insert(*actor, input.clone());
        }
        self.local_inputs.remove(&tick);
        let mut rolled = false;
        if tick >= self.head {
            sim.step(tick, &inputs, &commands);
            self.head = tick + 1;
            self.history.insert(self.head, sim.snapshot());
        } else if !commands.is_empty() || self.predicted.get(&tick) != Some(&inputs) {
            rolled = true;
            self.rollbacks += 1;
            let before = self
                .history
                .get(&tick)
                .expect("snapshot before every predicted tick");
            sim.restore(before);
            sim.step(tick, &inputs, &commands);
            self.history.insert(tick + 1, sim.snapshot());
            let old_head = self.head;
            self.head = tick + 1;
            self.predicted.split_off(&(tick + 1));
            self.resimulated_ticks += (old_head - self.head) as u64;
            self.predict(sim, old_head);
        }
        self.confirmed = tick + 1;
        self.predicted.remove(&tick);
        self.history.remove(&tick);
        rolled
    }

    fn guess(&self, tick: u32) -> TickInputs {
        let mut inputs = self.last_known.clone();
        match self.local_inputs.get(&tick).or(self.last_local.as_ref()) {
            Some(input) => {
                inputs.insert(self.local_actor, input.clone());
            }
            None => {
                inputs.remove(&self.local_actor);
            }
        }
        inputs
    }
}

/// Instant replay (kill cam) buffer: the inputs and commands of at least the last `keep` confirmed
/// ticks, back to a snapshot of the confirmed state taken every `every` ticks, so any recent moment
/// can be re-simulated locally without asking the node. Feed it from the confirmed path (`confirm` +
/// `confirmed_snapshot`).
pub struct ReplayRing<S> {
    keep: u32,
    every: u32,
    /// Tick of `inputs[0]`.
    first: u32,
    inputs: std::collections::VecDeque<TickInputs>,
    /// Commands of the same ticks as `inputs`.
    commands: std::collections::VecDeque<Vec<Command>>,
    /// State *before* tick `k` for boundaries `k` that are multiples of `every` (and the first one).
    snaps: BTreeMap<u32, S>,
}

/// What `ReplayRing::window` hands back: restore `snapshot`, then step from `start`, tick by tick,
/// with `inputs[i]` and `commands[i]`.
pub struct ReplayWindow<'a, S> {
    pub start: u32,
    pub snapshot: &'a S,
    pub inputs: Vec<&'a TickInputs>,
    pub commands: Vec<&'a [Command]>,
}

impl<S> ReplayRing<S> {
    pub fn new(keep_ticks: u32, snapshot_every: u32) -> Self {
        Self {
            keep: keep_ticks.max(1),
            every: snapshot_every.max(1),
            first: 0,
            inputs: std::collections::VecDeque::new(),
            commands: std::collections::VecDeque::new(),
            snaps: BTreeMap::new(),
        }
    }

    /// Records confirmed `tick` with its inputs as `LsEvent::Tick` delivers them, its commands
    /// among them, and the confirmed state *before* it (taken lazily only on snapshot boundaries).
    /// Ticks must arrive in order; a gap resets the ring.
    pub fn record(&mut self, tick: u32, mut inputs: TickInputs, state_before: impl FnOnce() -> S) {
        if self.inputs.is_empty() || self.first + self.inputs.len() as u32 != tick {
            self.inputs.clear();
            self.commands.clear();
            self.snaps.clear();
            self.first = tick;
        }
        if self.snaps.is_empty() || tick.is_multiple_of(self.every) {
            self.snaps.insert(tick, state_before());
        }
        self.commands.push_back(take_commands(&mut inputs));
        self.inputs.push_back(inputs);
        // the oldest tick kept is one with a snapshot, so every tick kept can be stepped from one
        let wanted = (tick + 1).saturating_sub(self.keep);
        if let Some((&anchor, _)) = self.snaps.range(..=wanted).next_back() {
            self.snaps.retain(|tick, _| *tick >= anchor);
            self.inputs.drain(..(anchor - self.first) as usize);
            self.commands.drain(..(anchor - self.first) as usize);
            self.first = anchor;
        }
    }

    /// Oldest tick that can still be replayed.
    pub fn oldest(&self) -> Option<u32> {
        self.snaps
            .keys()
            .next()
            .copied()
            .filter(|_| !self.inputs.is_empty())
    }

    /// Everything needed to re-simulate from the newest snapshot at or before `from` (clamped to
    /// what is kept) through the last recorded tick.
    pub fn window(&self, from: u32) -> Option<ReplayWindow<'_, S>> {
        let (&start, snapshot) = self
            .snaps
            .range(..=from)
            .next_back()
            .or_else(|| self.snaps.iter().next())?;
        let skip = start.saturating_sub(self.first) as usize;
        let inputs = self.inputs.iter().skip(skip).collect();
        let commands = self.commands.iter().skip(skip).map(Vec::as_slice).collect();
        Some(ReplayWindow {
            start,
            snapshot,
            inputs,
            commands,
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn replay_ring_rewinds_from_the_nearest_snapshot() {
        let mut ring: ReplayRing<u32> = ReplayRing::new(50, 10);
        for tick in 0..100u32 {
            ring.record(tick, inputs(&[(1, tick as u8)]), || tick * 100);
        }
        let window = ring.window(73).unwrap();
        assert_eq!(
            (window.start, *window.snapshot, window.inputs.len()),
            (70, 7000, 30)
        );
        assert_eq!(window.inputs[0][&1].as_ref(), &[70]);
        // older than what is kept: clamps to the oldest snapshot that still has all its inputs
        let window = ring.window(5).unwrap();
        assert_eq!((window.start, window.inputs.len()), (50, 50));
        assert_eq!(ring.oldest(), Some(50));
        // a gap resets
        ring.record(200, inputs(&[]), || 1);
        assert_eq!(
            (ring.oldest(), ring.window(0).unwrap().inputs.len()),
            (Some(200), 1)
        );
    }

    #[test]
    fn replay_ring_steps_a_window_with_the_inputs_of_its_own_ticks() {
        let mut ring: ReplayRing<u32> = ReplayRing::new(50, 10);
        for tick in 0..105u32 {
            ring.record(tick, inputs(&[(1, tick as u8)]), || tick * 100);
        }
        for from in [0, 52, 55, 60, 104] {
            let window = ring.window(from).unwrap();
            assert_eq!(*window.snapshot, window.start * 100);
            assert_eq!(window.inputs.len() as u32, 105 - window.start);
            assert_eq!(window.inputs[0][&1].as_ref(), &[window.start as u8]);
        }
        let oldest = ring.oldest().unwrap();
        assert_eq!(ring.window(oldest).unwrap().start, oldest);
        assert!(oldest <= 55);
    }

    use super::*;
    use crate::lockstep::COMMAND_ACTOR;

    /// Order-sensitive toy sim: state = fold over (tick, actor, input byte).
    #[derive(Default, Clone, PartialEq, Debug)]
    struct Toy {
        state: u64,
        steps: u64,
    }
    impl Simulation for Toy {
        type Snapshot = Toy;
        fn step(&mut self, tick: u32, inputs: &TickInputs, commands: &[Command]) {
            for command in commands {
                let byte = command.data.first().copied().unwrap_or(0) as u64;
                self.state = self
                    .state
                    .wrapping_mul(131)
                    .wrapping_add(command.actor as u64 * 1000 + byte);
            }
            for (actor, input) in inputs {
                let byte = input.first().copied().unwrap_or(0) as u64;
                self.state = self
                    .state
                    .wrapping_mul(31)
                    .wrapping_add(tick as u64 * 1000 + *actor as u64 * 10 + byte);
            }
            self.state = self.state.wrapping_mul(7).wrapping_add(tick as u64);
            self.steps += 1;
        }
        fn snapshot(&self) -> Toy {
            self.clone()
        }
        fn restore(&mut self, snapshot: &Toy) {
            *self = snapshot.clone();
        }
    }

    fn inputs(pairs: &[(u16, u8)]) -> TickInputs {
        pairs
            .iter()
            .map(|(actor, byte)| (*actor, Bytes::from(vec![*byte])))
            .collect()
    }

    /// The entry of `COMMAND_ACTOR` with one command of one byte.
    fn command_entry(actor: u16, byte: u8) -> Bytes {
        let [high, low] = actor.to_be_bytes();
        Bytes::from(vec![high, low, 0, 1, byte])
    }

    fn reference(script: &[TickInputs]) -> u64 {
        let mut sim = Toy::default();
        for (tick, inputs) in script.iter().enumerate() {
            let mut inputs = inputs.clone();
            let commands = take_commands(&mut inputs);
            sim.step(tick as u32, &inputs, &commands);
        }
        sim.state
    }

    #[test]
    fn correct_predictions_never_roll_back() {
        let mut sim = Toy::default();
        let mut rb = Rollback::new(&sim, 1, 0, 8);
        let script: Vec<TickInputs> = (0..10).map(|_| inputs(&[(1, 5), (2, 9)])).collect();
        rb.confirm(&mut sim, 0, &script[0]);
        for tick in 1..10u32 {
            rb.local_input(tick, Bytes::from(vec![5]));
            rb.predict(&mut sim, tick + 3);
            assert!(!rb.confirm(&mut sim, tick, &script[tick as usize]));
        }
        assert_eq!(rb.rollbacks, 0);
        assert_eq!(rb.confirmed_tick(), 10);
        // head ran ahead; the confirmed snapshot equals a pure confirmed simulation
        sim.restore(rb.confirmed_snapshot());
        assert_eq!(sim.state, reference(&script));
    }

    #[test]
    fn mispredicted_remote_input_rolls_back_to_the_same_state_as_pure_lockstep() {
        let mut sim = Toy::default();
        let mut rb = Rollback::new(&sim, 1, 0, 8);
        let mut script: Vec<TickInputs> = (0..8).map(|_| inputs(&[(1, 1), (2, 2)])).collect();
        script[4] = inputs(&[(1, 1), (2, 7)]); // remote actor changes input at tick 4
        script[5] = inputs(&[(2, 7)]); // and our own input arrives late (dropped from tick 5)
        for tick in 0..8u32 {
            rb.local_input(tick, Bytes::from(vec![1]));
            rb.predict(&mut sim, tick + 4);
            rb.confirm(&mut sim, tick, &script[tick as usize]);
        }
        // ticks 0 (actor 2 unknown yet), 4, 5 and 6: identical across the Rust/Web/C# cores
        assert_eq!(rb.rollbacks, 4);
        assert!(rb.resimulated_ticks > 0);
        sim.restore(rb.confirmed_snapshot());
        assert_eq!(sim.state, reference(&script));
        assert_eq!(rb.head(), 11);
    }

    #[test]
    fn a_confirmed_tick_with_a_command_is_stepped_again_with_it() {
        let mut sim = Toy::default();
        let mut rb = Rollback::new(&sim, 1, 0, 8);
        let mut script: Vec<TickInputs> = (0..8).map(|_| inputs(&[(1, 5), (2, 9)])).collect();
        script[4].insert(COMMAND_ACTOR, command_entry(2, 3));
        rb.confirm(&mut sim, 0, &script[0]);
        for tick in 1..8u32 {
            rb.local_input(tick, Bytes::from(vec![5]));
            rb.predict(&mut sim, tick + 3);
            let rolled = rb.confirm(&mut sim, tick, &script[tick as usize]);
            assert_eq!(rolled, tick == 4, "tick {tick}");
        }
        assert_eq!(rb.rollbacks, 1);
        sim.restore(rb.confirmed_snapshot());
        assert_eq!(sim.state, reference(&script));
    }

    /// Keeps what every step was given.
    #[derive(Default)]
    struct Journal {
        steps: Vec<(u32, Vec<u16>, Vec<Command>)>,
    }
    impl Simulation for Journal {
        type Snapshot = usize;
        fn step(&mut self, tick: u32, inputs: &TickInputs, commands: &[Command]) {
            let actors = inputs.keys().copied().collect();
            self.steps.push((tick, actors, commands.to_vec()));
        }
        fn snapshot(&self) -> usize {
            self.steps.len()
        }
        fn restore(&mut self, snapshot: &usize) {
            self.steps.truncate(*snapshot);
        }
    }

    #[test]
    fn a_command_is_not_repeated_in_predicted_ticks() {
        let mut sim = Journal::default();
        let mut rb = Rollback::new(&sim, 1, 0, 8);
        let mut tick = inputs(&[(1, 5), (2, 9)]);
        rb.confirm(&mut sim, 0, &tick);
        tick.insert(COMMAND_ACTOR, command_entry(2, 3));
        rb.confirm(&mut sim, 1, &tick);
        rb.local_input(2, Bytes::from(vec![5]));
        rb.predict(&mut sim, 6);

        let mut expected: Vec<_> = (0..6u32).map(|tick| (tick, vec![1, 2], vec![])).collect();
        expected[1].2.push(Command {
            actor: 2,
            data: Bytes::from_static(&[3]),
        });
        assert_eq!(sim.steps, expected);
    }

    #[test]
    fn replay_ring_keeps_the_commands_of_a_tick_with_its_inputs() {
        let mut ring: ReplayRing<u32> = ReplayRing::new(50, 10);
        for tick in 0..30u32 {
            let mut inputs = inputs(&[(1, tick as u8)]);
            if tick == 23 {
                inputs.insert(COMMAND_ACTOR, command_entry(1, 7));
            }
            ring.record(tick, inputs, || tick);
        }
        let window = ring.window(21).unwrap();
        assert_eq!(window.start, 20);
        assert_eq!(window.commands.len(), window.inputs.len());
        for (index, commands) in window.commands.iter().enumerate() {
            let expected: &[Command] = if index == 3 {
                &[Command {
                    actor: 1,
                    data: Bytes::from_static(&[7]),
                }]
            } else {
                &[]
            };
            assert_eq!(*commands, expected, "tick {}", 20 + index);
            assert_eq!(
                window.inputs[index].keys().copied().collect::<Vec<_>>(),
                [1]
            );
        }
    }

    #[test]
    fn prediction_is_bounded_and_reset_forgets_history() {
        let mut sim = Toy::default();
        let mut rb = Rollback::new(&sim, 1, 0, 3);
        rb.predict(&mut sim, 100);
        assert_eq!(rb.head(), 3);
        assert_eq!(rb.predicted_ticks, 3);
        assert!(
            !rb.confirm(&mut sim, 7, &inputs(&[])),
            "out-of-order confirm is ignored"
        );
        sim = Toy {
            state: 42,
            steps: 0,
        };
        rb.reset(&sim, 50);
        assert_eq!((rb.confirmed_tick(), rb.head()), (50, 50));
        rb.predict(&mut sim, 52);
        assert_eq!(rb.head(), 52);
        assert_eq!(rb.history.len(), 3);
    }
}
