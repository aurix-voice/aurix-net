// Client-side prediction with server reconciliation (PurrDiction-style API). Subclass with an INPUT
// and a STATE struct, implement Simulate(input, ref state, dt) deterministically from those alone:
//   owner    : samples input every tick (GetInput), simulates at once, keeps a history, sends inputs
//              to the server (redundantly), and on each verified state compares it with its own for
//              that tick; on a mismatch it rewinds and re-simulates the inputs after it;
//   server   : simulates the owner's input for each tick (repeats the last one when it is late),
//              sends the verified state to observers every stateSendInterval ticks;
//   observers: interpolate between verified states (UpdateView).
// Host-owned objects simulate on the server directly (no prediction needed).
using System;
using System.Collections.Generic;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    /// <summary>Owner → server: a window of recent inputs, newest last (redundancy against loss).</summary>
    public sealed class InputStream<INPUT> : NetworkModule
    {
        public readonly List<(uint tick, INPUT input)> pending = new List<(uint, INPUT)>();
        public readonly Queue<(uint tick, INPUT input)> received = new Queue<(uint, INPUT)>();
        public int redundancy = 3;

        public InputStream()
        {
            ownerAuth = true;
            serverOnly = true;
        }

        public void Push(uint tick, INPUT input)
        {
            pending.Add((tick, input));
            while (pending.Count > redundancy)
            {
                pending.RemoveAt(0);
            }

            isDirty = true;
        }

        public override void WriteFull(BitWriter writer) => writer.WriteBits(0, 4);

        public override void ReadFull(BitReader reader) => reader.ReadBits(4);

        public override void WriteDelta(BitWriter writer)
        {
            writer.WriteBits((ulong)pending.Count, 4);
            foreach (var (tick, input) in pending)
            {
                writer.WriteBits(tick, 32);
                NetSerializer.Write(writer, input, typeof(INPUT));
            }
        }

        public override void ReadDelta(BitReader reader)
        {
            int count = (int)reader.ReadBits(4);
            for (int k = 0; k < count; k++)
            {
                uint tick = (uint)reader.ReadBits(32);
                var input = (INPUT)NetSerializer.Read(reader, typeof(INPUT));
                received.Enqueue((tick, input));
            }
        }
    }

    /// <summary>Server → observers: the verified state at a tick.</summary>
    public sealed class VerifiedState<STATE> : NetworkModule
    {
        public uint tick;
        public STATE state;
        public bool has;
        public event Action<uint, STATE> onReceived;

        public void Set(uint newTick, STATE newState)
        {
            tick = newTick;
            state = newState;
            has = true;
            isDirty = true;
        }

        public override void WriteFull(BitWriter writer)
        {
            writer.WriteBool(has);
            if (has)
            {
                writer.WriteBits(tick, 32);
                NetSerializer.Write(writer, state, typeof(STATE));
            }
        }

        public override void ReadFull(BitReader reader)
        {
            if (!reader.ReadBool())
            {
                return;
            }

            tick = (uint)reader.ReadBits(32);
            state = (STATE)NetSerializer.Read(reader, typeof(STATE));
            has = true;
            onReceived?.Invoke(tick, state);
        }

        public override void WriteDelta(BitWriter writer) => WriteFull(writer);

        public override void ReadDelta(BitReader reader) => ReadFull(reader);
    }

    public abstract class PredictedBehaviour<INPUT, STATE> : NetworkBehaviour
        where INPUT : struct
        where STATE : struct
    {
        [Tooltip(
            "Ticks of slack the owner keeps on top of the measured round trip when it simulates ahead of the newest server tick."
        )]
        public int inputLeadTicks = 3;

        [Tooltip("Server sends the verified state every N ticks.")]
        public int stateSendInterval = 1;

        [Tooltip("History kept on the owner for reconciliation.")]
        public int historyTicks = 128;

        public InputStream<INPUT> inputs;
        public VerifiedState<STATE> verified;

        /// <summary>Current simulated state: predicted on the owner, authoritative on the server, latest verified on observers.</summary>
        public STATE state;
        public uint currentTick { get; private set; }
        public int reconciliations { get; private set; }
        public int missedInputs { get; private set; }
        public uint lastVerifiedTick => verified != null && verified.has ? verified.tick : 0;

        readonly SortedDictionary<uint, (INPUT input, STATE after)> _history =
            new SortedDictionary<uint, (INPUT, STATE)>();
        INPUT _lastServerInput;
        bool _hasServerInput;
        readonly SortedDictionary<uint, INPUT> _serverInputs = new SortedDictionary<uint, INPUT>();
        STATE _viewFrom,
            _viewTo;
        uint _viewFromTick,
            _viewToTick;
        float _viewT;
        bool _viewInit;

        protected override void OnBeforeBind()
        {
            inputs ??= new InputStream<INPUT>();
            verified ??= new VerifiedState<STATE>();
            verified.onReceived += OnVerified;
        }

        protected override void OnSpawned(bool asServer)
        {
            if (isServer)
            {
                state = GetInitialState();
                currentTick = networkManager.tick;
                verified.Set(currentTick, state);
            }
            else if (verified.has)
            {
                state = verified.state;
                currentTick = verified.tick;
                _viewFrom = _viewTo = state;
                _viewFromTick = _viewToTick = verified.tick;
                _viewInit = true;
            }
            else
            {
                state = GetInitialState();
            }

            SetUnityState(state);
        }

        // ---- to implement ----
        /// <summary>Owner: fill this tick's input (keys, aim…).</summary>
        protected virtual void GetInput(ref INPUT input) { }

        protected virtual INPUT GetDefaultInput() => default;

        /// <summary>Deterministic step: must depend only on input and state.</summary>
        protected abstract void Simulate(INPUT input, ref STATE state, float delta);

        protected virtual STATE GetInitialState() => default;

        /// <summary>Server: hook to apply authoritative changes (hits, knockback) before the step.</summary>
        protected virtual void ServerModifyState(ref STATE state) { }

        /// <summary>Push the state into the scene (transform, animator). Called for the owner after each step and for observers each frame.</summary>
        protected virtual void SetUnityState(STATE state) { }

        /// <summary>Observers: blend between verified states (default: step to the newer one).</summary>
        protected virtual STATE Interpolate(STATE from, STATE to, float fraction) =>
            fraction < 1f ? from : to;

        protected virtual void UpdateView(STATE viewState, STATE? verifiedState) =>
            SetUnityState(viewState);

        protected virtual bool StatesEqual(in STATE left, in STATE right) =>
            EqualityComparer<STATE>.Default.Equals(left, right);

        protected virtual void OnReconciled(
            uint tick,
            in STATE predicted,
            in STATE verifiedState
        ) { }

        // ---- tick ----
        public override void OnTick(uint tick)
        {
            if (!isSpawned)
            {
                return;
            }

            float dt = 1f / Math.Max(1, networkManager.tickRate);
            if (isServer)
            {
                ServerTick(tick, dt);
            }
            else if (isOwner)
            {
                OwnerTick(dt);
            }
        }

        void ServerTick(uint tick, float dt)
        {
            while (inputs.received.Count > 0)
            {
                var (inputTick, received) = inputs.received.Dequeue();
                if (inputTick >= currentTick)
                {
                    _serverInputs[inputTick] = received;
                }
            }
            INPUT input;
            if (identity.hasOwner && !identity.isOwner)
            {
                if (_serverInputs.TryGetValue(tick, out var got))
                {
                    input = got;
                    _lastServerInput = got;
                    _hasServerInput = true;
                }
                else
                {
                    input = _hasServerInput ? _lastServerInput : GetDefaultInput();
                    missedInputs++;
                }
                foreach (var key in new List<uint>(_serverInputs.Keys))
                {
                    if (key <= tick)
                    {
                        _serverInputs.Remove(key);
                    }
                }
            }
            else
            {
                input = GetDefaultInput();
                GetInput(ref input);
            }
            ServerModifyState(ref state);
            Simulate(input, ref state, dt);
            currentTick = tick;
            if (stateSendInterval <= 1 || tick % (uint)stateSendInterval == 0)
            {
                verified.Set(tick, state);
            }

            SetUnityState(state);
        }

        void OwnerTick(float dt)
        {
            // an input has to reach the server before the server simulates its tick
            var statistics = networkManager.statistics;
            int roundTrip = statistics.hasPingEstimate
                ? (int)Math.Ceiling(statistics.ping * networkManager.tickRate / 1000f)
                : 0;
            uint target =
                networkManager.serverTick + (uint)(Math.Max(0, inputLeadTicks) + roundTrip);
            if (currentTick == 0 || target < currentTick)
            {
                currentTick = target == 0 ? 1 : target;
            }
            // advance at least one tick per local tick; catch up when the server moved further than expected
            uint until = Math.Max(currentTick + 1, target);
            int steps = 0;
            while (currentTick < until && steps++ < 8)
            {
                currentTick++;
                var input = GetDefaultInput();
                GetInput(ref input);
                Simulate(input, ref state, dt);
                _history[currentTick] = (input, state);
                inputs.Push(currentTick, input);
            }
            while (_history.Count > historyTicks)
            {
                _history.Remove(First(_history));
            }

            SetUnityState(state);
        }

        static uint First(SortedDictionary<uint, (INPUT, STATE)> history)
        {
            foreach (var key in history.Keys)
            {
                return key;
            }

            return 0;
        }

        void OnVerified(uint tick, STATE server)
        {
            if (!isSpawned || isServer)
            {
                return;
            }

            if (!isOwner)
            {
                if (!_viewInit)
                {
                    _viewFrom = _viewTo = server;
                    _viewFromTick = _viewToTick = tick;
                    _viewInit = true;
                }
                else
                {
                    _viewFrom = Interpolate(_viewFrom, _viewTo, _viewT);
                    _viewFromTick = _viewToTick;
                    _viewTo = server;
                    _viewToTick = tick;
                    _viewT = 0f;
                }
                state = server;
                currentTick = tick;
                return;
            }
            if (!_history.TryGetValue(tick, out var entry))
            {
                if (tick > currentTick)
                {
                    state = server;
                    currentTick = tick;
                    _history.Clear();
                }
                return;
            }
            if (StatesEqual(entry.after, server))
            {
                foreach (var key in new List<uint>(_history.Keys))
                {
                    if (key < tick)
                    {
                        _history.Remove(key);
                    }
                }

                return;
            }
            // mismatch: rewind to the server's state and replay the inputs after that tick
            reconciliations++;
            var predicted = entry.after;
            state = server;
            float dt = 1f / Math.Max(1, networkManager.tickRate);
            foreach (var key in new List<uint>(_history.Keys))
            {
                if (key <= tick)
                {
                    _history.Remove(key);
                    continue;
                }
                var recorded = _history[key];
                Simulate(recorded.input, ref state, dt);
                _history[key] = (recorded.input, state);
            }
            OnReconciled(tick, predicted, server);
            SetUnityState(state);
        }

        /// <summary>Observers: call every frame (Update) to advance the view between verified states.</summary>
        public void RenderUpdate(float dt)
        {
            if (!isSpawned || isServer || isOwner || !_viewInit)
            {
                return;
            }

            float span =
                Math.Max(1, (float)(_viewToTick - _viewFromTick))
                / Math.Max(1, networkManager.tickRate);
            _viewT = Math.Min(1f, _viewT + dt / span);
            UpdateView(
                Interpolate(_viewFrom, _viewTo, _viewT),
                verified.has ? verified.state : (STATE?)null
            );
        }

        protected virtual void Update() => RenderUpdate(Time.unscaledDeltaTime);
    }
}
