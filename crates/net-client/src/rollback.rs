//! Predict/rollback on top of lockstep: the game runs ahead of the confirmed tick with predicted
//! remote inputs (each actor's last confirmed input, repeated) and its own inputs as sent, keeps a
//! snapshot per predicted tick, and when the node confirms a tick whose inputs differ from the
//! prediction it restores the snapshot, re-steps that tick and re-predicts the ticks after it.
//! Local actions therefore render immediately; the simulation stays deterministic because only
//! confirmed inputs ever reach the confirmed state. Same core as the C#/Web `LockstepRollback`.

use std::collections::BTreeMap;

use bytes::Bytes;

use crate::lockstep::TickInputs;

/// The game's deterministic simulation: step by one tick, snapshot and restore.
pub trait Simulation {
    type Snapshot;
    fn step(&mut self, tick: u32, inputs: &TickInputs);
    fn snapshot(&self) -> Self::Snapshot;
    fn restore(&mut self, s: &Self::Snapshot);
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
            let t = self.head;
            let inputs = self.guess(t);
            sim.step(t, &inputs);
            self.predicted.insert(t, inputs);
            self.head = t + 1;
            self.history.insert(self.head, sim.snapshot());
            self.predicted_ticks += 1;
        }
    }

    /// Feeds a confirmed tick (`LsEvent::Tick`). Returns true when a rollback happened.
    pub fn confirm(&mut self, sim: &mut S, tick: u32, inputs: &TickInputs) -> bool {
        if tick != self.confirmed {
            return false;
        }
        for (a, i) in inputs {
            self.last_known.insert(*a, i.clone());
        }
        self.local_inputs.remove(&tick);
        let mut rolled = false;
        if tick >= self.head {
            sim.step(tick, inputs);
            self.head = tick + 1;
            self.history.insert(self.head, sim.snapshot());
        } else if self.predicted.get(&tick) != Some(inputs) {
            rolled = true;
            self.rollbacks += 1;
            let before = self
                .history
                .get(&tick)
                .expect("snapshot before every predicted tick");
            sim.restore(before);
            sim.step(tick, inputs);
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
        let mut g = self.last_known.clone();
        match self.local_inputs.get(&tick).or(self.last_local.as_ref()) {
            Some(i) => {
                g.insert(self.local_actor, i.clone());
            }
            None => {
                g.remove(&self.local_actor);
            }
        }
        g
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Order-sensitive toy sim: state = fold over (tick, actor, input byte).
    #[derive(Default, Clone, PartialEq, Debug)]
    struct Toy {
        state: u64,
        steps: u64,
    }
    impl Simulation for Toy {
        type Snapshot = Toy;
        fn step(&mut self, tick: u32, inputs: &TickInputs) {
            for (a, i) in inputs {
                let b = i.first().copied().unwrap_or(0) as u64;
                self.state = self
                    .state
                    .wrapping_mul(31)
                    .wrapping_add(tick as u64 * 1000 + *a as u64 * 10 + b);
            }
            self.state = self.state.wrapping_mul(7).wrapping_add(tick as u64);
            self.steps += 1;
        }
        fn snapshot(&self) -> Toy {
            self.clone()
        }
        fn restore(&mut self, s: &Toy) {
            *self = s.clone();
        }
    }

    fn inputs(v: &[(u16, u8)]) -> TickInputs {
        v.iter().map(|(a, b)| (*a, Bytes::from(vec![*b]))).collect()
    }

    fn reference(script: &[TickInputs]) -> u64 {
        let mut s = Toy::default();
        for (t, i) in script.iter().enumerate() {
            s.step(t as u32, i);
        }
        s.state
    }

    #[test]
    fn correct_predictions_never_roll_back() {
        let mut sim = Toy::default();
        let mut rb = Rollback::new(&sim, 1, 0, 8);
        let script: Vec<TickInputs> = (0..10).map(|_| inputs(&[(1, 5), (2, 9)])).collect();
        rb.confirm(&mut sim, 0, &script[0]);
        for t in 1..10u32 {
            rb.local_input(t, Bytes::from(vec![5]));
            rb.predict(&mut sim, t + 3);
            assert!(!rb.confirm(&mut sim, t, &script[t as usize]));
        }
        assert_eq!(rb.rollbacks, 0);
        assert_eq!(rb.confirmed_tick(), 10);
        // head ran ahead; the confirmed snapshot equals a pure confirmed simulation
        sim.restore(&rb.history[&rb.confirmed_tick()]);
        assert_eq!(sim.state, reference(&script));
    }

    #[test]
    fn mispredicted_remote_input_rolls_back_to_the_same_state_as_pure_lockstep() {
        let mut sim = Toy::default();
        let mut rb = Rollback::new(&sim, 1, 0, 8);
        let mut script: Vec<TickInputs> = (0..8).map(|_| inputs(&[(1, 1), (2, 2)])).collect();
        script[4] = inputs(&[(1, 1), (2, 7)]); // remote actor changes input at tick 4
        script[5] = inputs(&[(2, 7)]); // and our own input arrives late (dropped from tick 5)
        for t in 0..8u32 {
            rb.local_input(t, Bytes::from(vec![1]));
            rb.predict(&mut sim, t + 4);
            rb.confirm(&mut sim, t, &script[t as usize]);
        }
        // ticks 0 (actor 2 unknown yet), 4, 5 and 6: identical across the Rust/Web/C# cores
        assert_eq!(rb.rollbacks, 4);
        assert!(rb.resimulated_ticks > 0);
        sim.restore(&rb.history[&rb.confirmed_tick()]);
        assert_eq!(sim.state, reference(&script));
        assert_eq!(rb.head(), 11);
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
