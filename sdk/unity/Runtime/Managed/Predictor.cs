// Client-side prediction for server-authoritative entities (sync rooms with a WASM `logic` module).
// The local player applies its own input immediately with the same step function the server runs,
// keeps the inputs it has not seen confirmed yet, and when the server's state for tick T arrives it
// re-simulates every later input on top of it — so a mismatch (hit a wall the client did not know
// about, got pushed) is corrected without waiting a round trip, and a correct guess costs nothing.
using System;
using System.Collections.Generic;

namespace Aurix.Net.Managed
{
    public sealed class Predictor<TState>
    {
        struct Frame { public uint Tick; public byte[] Input; public TState After; }

        readonly Func<TState, byte[], TState> _step;
        readonly Func<TState, TState, bool> _diverged;
        readonly List<Frame> _pending = new List<Frame>();
        readonly int _capacity;
        TState _confirmed;

        /// <summary>State after the newest local input (what to render for the local player).</summary>
        public TState Predicted { get; private set; }
        /// <summary>Last server tick that was reconciled.</summary>
        public uint ConfirmedTick { get; private set; }
        /// <summary>How many inputs are in flight (sent, not yet confirmed by a server state).</summary>
        public int Pending => _pending.Count;
        /// <summary>Corrections applied so far (server state differed from the prediction).</summary>
        public long Mispredictions { get; private set; }
        /// <summary>Fires after a correction with the state the client had predicted for that tick and the
        /// authoritative one; use it to smooth the visual snap (error offset decayed over a few frames).</summary>
        public event Action<TState, TState> OnCorrected;

        /// <param name="step">Deterministic simulation step: state + one tick of input → next state. Must be
        /// the same rule the server logic applies for this entity.</param>
        /// <param name="diverged">Returns true when the authoritative state differs from the prediction
        /// enough to replay (null = always replay; cheap for small states).</param>
        /// <param name="capacity">Max inputs kept in flight; older ones are dropped (the server is that far
        /// behind, prediction is meaningless anyway).</param>
        public Predictor(TState initial, Func<TState, byte[], TState> step, Func<TState, TState, bool> diverged = null, int capacity = 128)
        {
            Predicted = initial; _confirmed = initial; _step = step ?? throw new ArgumentNullException(nameof(step)); _diverged = diverged; _capacity = Math.Max(1, capacity);
        }

        /// <summary>Applies a local input for <paramref name="tick"/> (the tick you sent it with) and remembers it.</summary>
        public TState Record(uint tick, byte[] input)
        {
            Predicted = _step(Predicted, input);
            _pending.Add(new Frame { Tick = tick, Input = input, After = Predicted });
            while (_pending.Count > _capacity) _pending.RemoveAt(0);
            return Predicted;
        }

        /// <summary>Feeds the authoritative state the server produced after consuming inputs up to
        /// <paramref name="tick"/>. Returns true when the prediction for that tick was wrong and the
        /// pending inputs were replayed on top of the server state.</summary>
        public bool Reconcile(uint tick, TState authoritative)
        {
            if (tick < ConfirmedTick) return false; // stale (reordered) state
            ConfirmedTick = tick;
            int confirmed = 0;
            while (confirmed < _pending.Count && _pending[confirmed].Tick <= tick) confirmed++;
            TState predictedThen = confirmed > 0 ? _pending[confirmed - 1].After : _confirmed;
            _pending.RemoveRange(0, confirmed);
            _confirmed = authoritative;
            bool wrong = _diverged != null ? _diverged(authoritative, predictedThen) : !Equals(authoritative, predictedThen);
            if (!wrong) return false;
            var s = authoritative;
            for (int i = 0; i < _pending.Count; i++) { s = _step(s, _pending[i].Input); var f = _pending[i]; f.After = s; _pending[i] = f; }
            Predicted = s;
            Mispredictions++;
            OnCorrected?.Invoke(predictedThen, authoritative);
            return true;
        }

        /// <summary>Forget everything (teleport, respawn, rehost).</summary>
        public void Reset(TState state) { _pending.Clear(); Predicted = state; _confirmed = state; }
    }
}
