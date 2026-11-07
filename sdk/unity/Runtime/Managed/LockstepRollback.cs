// Predict/rollback on top of LockstepClient (same core as the Rust/Web SDKs): the game runs ahead of
// the confirmed tick with predicted remote inputs (each actor's last confirmed input, repeated) and
// its own inputs as sent, keeps a snapshot per predicted tick, and when a confirmed tick differs from
// the prediction restores the snapshot, re-steps it and re-predicts the ticks after it.
using System;
using System.Collections.Generic;
using System.Linq;

namespace Aurix.Net.Managed
{
    /// <summary>The game's deterministic simulation: step one tick, snapshot, restore.</summary>
    public interface ISimulation<TSnapshot>
    {
        void Step(uint tick, Dictionary<ushort, byte[]> inputs);
        TSnapshot Snapshot();
        void Restore(TSnapshot s);
    }

    public sealed class LockstepRollback<TSnapshot>
    {
        readonly ISimulation<TSnapshot> _sim; readonly ushort _localActor;
        readonly Dictionary<uint, TSnapshot> _history = new Dictionary<uint, TSnapshot>();
        readonly Dictionary<uint, Dictionary<ushort, byte[]>> _predicted = new Dictionary<uint, Dictionary<ushort, byte[]>>();
        readonly Dictionary<uint, byte[]> _localInputs = new Dictionary<uint, byte[]>();
        readonly Dictionary<ushort, byte[]> _lastKnown = new Dictionary<ushort, byte[]>();
        byte[] _lastLocal;

        /// <summary>Ticks the simulation may run ahead of the confirmed tick.</summary>
        public uint MaxPrediction;
        /// <summary>First tick not yet confirmed by the node.</summary>
        public uint ConfirmedTick { get; private set; }
        /// <summary>Next tick the simulation will run; HeadTick - ConfirmedTick ticks are predicted.</summary>
        public uint HeadTick { get; private set; }
        public long Rollbacks, ResimulatedTicks, PredictedTicks;
        /// <summary>State after the last confirmed tick: hash <em>this</em> for ReportHash, never the predicted head.</summary>
        public TSnapshot ConfirmedSnapshot => _history[ConfirmedTick];

        /// <param name="sim">Must be at tick <paramref name="start"/> (fresh game, or restored from a catch-up checkpoint).</param>
        public LockstepRollback(ISimulation<TSnapshot> sim, ushort localActor, uint start = 0, uint maxPrediction = 8)
        {
            _sim = sim; _localActor = localActor; MaxPrediction = Math.Max(1u, maxPrediction);
            ConfirmedTick = HeadTick = start; _history[start] = sim.Snapshot();
        }

        /// <summary>After the game restored a catch-up checkpoint at <paramref name="tick"/>: forget every prediction.</summary>
        public void Reset(uint tick)
        {
            _history.Clear(); _history[tick] = _sim.Snapshot(); _predicted.Clear(); _localInputs.Clear();
            ConfirmedTick = HeadTick = tick;
        }

        /// <summary>Remember the input sent stamped with <paramref name="tick"/> (what LockstepClient.SendInput stamped) so our own prediction uses exactly it.</summary>
        public void LocalInput(uint tick, byte[] input) { _lastLocal = input; _localInputs[tick] = input; }

        /// <summary>Runs the simulation ahead to <paramref name="target"/> (exclusive), bounded by MaxPrediction.</summary>
        public void Predict(uint target)
        {
            target = Math.Min(target, ConfirmedTick + Math.Max(1u, MaxPrediction));
            while (HeadTick < target)
            {
                uint t = HeadTick; var inputs = Guess(t);
                _sim.Step(t, inputs); _predicted[t] = inputs; HeadTick = t + 1; _history[HeadTick] = _sim.Snapshot(); PredictedTicks++;
            }
        }

        /// <summary>Feeds a confirmed tick (LockstepClient.OnTick). True when a rollback happened.</summary>
        public bool Confirm(uint tick, Dictionary<ushort, byte[]> inputs)
        {
            if (tick != ConfirmedTick) return false;
            foreach (var kv in inputs) _lastKnown[kv.Key] = kv.Value;
            _localInputs.Remove(tick);
            bool rolled = false;
            if (tick >= HeadTick) { _sim.Step(tick, inputs); HeadTick = tick + 1; _history[HeadTick] = _sim.Snapshot(); }
            else if (!_predicted.TryGetValue(tick, out var guess) || !SameInputs(guess, inputs))
            {
                rolled = true; Rollbacks++;
                _sim.Restore(_history[tick]); _sim.Step(tick, inputs); _history[tick + 1] = _sim.Snapshot();
                uint oldHead = HeadTick; HeadTick = tick + 1;
                foreach (var t in _predicted.Keys.Where(k => k > tick).ToList()) _predicted.Remove(t);
                ResimulatedTicks += oldHead - HeadTick;
                Predict(oldHead);
            }
            ConfirmedTick = tick + 1; _predicted.Remove(tick); _history.Remove(tick);
            return rolled;
        }

        Dictionary<ushort, byte[]> Guess(uint tick)
        {
            var g = new Dictionary<ushort, byte[]>(_lastKnown);
            var mine = _localInputs.TryGetValue(tick, out var exact) ? exact : _lastLocal;
            if (mine != null) g[_localActor] = mine; else g.Remove(_localActor);
            return g;
        }

        static bool SameInputs(Dictionary<ushort, byte[]> a, Dictionary<ushort, byte[]> b)
        {
            if (a.Count != b.Count) return false;
            foreach (var kv in a) { if (!b.TryGetValue(kv.Key, out var w) || !w.SequenceEqual(kv.Value)) return false; }
            return true;
        }
    }

    /// <summary>What <see cref="ReplayRing{TSnapshot}.Window"/> hands back: restore Snapshot, then step Inputs from Start.</summary>
    public struct ReplayWindow<TSnapshot> { public uint Start; public TSnapshot Snapshot; public List<Dictionary<ushort, byte[]>> Inputs; }

    /// <summary>Instant replay (kill cam) buffer: the last <c>keep</c> confirmed ticks' inputs plus a snapshot of the confirmed state every <c>every</c> ticks. Feed it from the confirmed path (after Confirm, with ConfirmedSnapshot).</summary>
    public sealed class ReplayRing<TSnapshot>
    {
        readonly int _keep; readonly uint _every; uint _first;
        readonly List<Dictionary<ushort, byte[]>> _inputs = new List<Dictionary<ushort, byte[]>>();
        readonly SortedDictionary<uint, TSnapshot> _snaps = new SortedDictionary<uint, TSnapshot>();
        public ReplayRing(int keepTicks = 300, uint snapshotEvery = 10) { _keep = Math.Max(1, keepTicks); _every = Math.Max(1u, snapshotEvery); }

        /// <summary>Records confirmed <paramref name="tick"/> with its inputs and the confirmed state <em>before</em> it (taken lazily on snapshot boundaries). A gap resets the ring.</summary>
        public void Record(uint tick, Dictionary<ushort, byte[]> inputs, Func<TSnapshot> stateBefore)
        {
            if (_inputs.Count == 0 || _first + (uint)_inputs.Count != tick) { _inputs.Clear(); _snaps.Clear(); _first = tick; }
            if (_snaps.Count == 0 || tick % _every == 0) _snaps[tick] = stateBefore();
            _inputs.Add(inputs);
            if (_inputs.Count > _keep) { int drop = _inputs.Count - _keep; _inputs.RemoveRange(0, drop); _first += (uint)drop; }
            uint anchor = 0; bool found = false;
            foreach (var k in _snaps.Keys) { if (k <= _first) { anchor = k; found = true; } else break; }
            if (found) foreach (var k in _snaps.Keys.Where(k => k < anchor).ToList()) _snaps.Remove(k);
        }
        /// <summary>Oldest tick that can still be replayed, or null.</summary>
        public uint? Oldest => _inputs.Count == 0 ? (uint?)null : _snaps.Keys.First();
        /// <summary>Everything needed to re-simulate from the newest snapshot at or before <paramref name="from"/> (clamped to what is kept) through the last recorded tick.</summary>
        public ReplayWindow<TSnapshot>? Window(uint from)
        {
            if (_inputs.Count == 0) return null;
            uint start = _snaps.Keys.First();
            foreach (var k in _snaps.Keys) { if (k <= from) start = k; else break; }
            int skip = (int)Math.Max(0, (long)start - _first);
            return new ReplayWindow<TSnapshot> { Start = start, Snapshot = _snaps[start], Inputs = _inputs.Skip(skip).ToList() };
        }
    }
}
