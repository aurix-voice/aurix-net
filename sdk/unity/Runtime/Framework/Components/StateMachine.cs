// Aurix Net Unity SDK — replicated state machine (PurrNet-style StateMachine / StateNode).
// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using UnityEngine;
using Aurix.Net.Managed;

namespace Aurix.Net.Unity.Framework
{
    /// <summary>A state of a <see cref="StateMachine"/>. Add instances to <c>StateMachine.states</c>; order must match on every peer.</summary>
    public abstract class StateNode : NetworkBehaviour
    {
        public StateMachine machine { get; internal set; }
        public bool isCurrentState => machine != null && machine.currentStateNode == this;

        public virtual void Enter() { }
        public virtual void Enter(bool asServer) { }
        public virtual void StateUpdate() { }
        public virtual void StateUpdate(bool asServer) { }
        public virtual void Exit() { }
        public virtual void Exit(bool asServer) { }
        public virtual bool CanEnter() => true;
        public virtual bool CanExit() => true;

        internal virtual void EnterWithData(BitReader data, bool asServer) { Enter(); Enter(asServer); }
        internal virtual Type dataType => null;
    }

    /// <summary>A state that is entered with a payload of type <typeparamref name="T"/> (replicated to every peer and to late joiners).</summary>
    public abstract class StateNode<T> : StateNode
    {
        public T lastData { get; private set; }
        public virtual void Enter(T data) { }
        public virtual void Enter(T data, bool asServer) { }
        public virtual bool CanEnter(T data) => true;

        internal override Type dataType => typeof(T);
        internal override void EnterWithData(BitReader data, bool asServer)
        {
            lastData = data != null ? NetSerializer.Read<T>(data) : default;
            Enter(); Enter(asServer); Enter(lastData); Enter(lastData, asServer);
        }
    }

    /// <summary>
    /// Replicated state machine: the controller (server, or owner when <c>ownerAuth</c>) switches states, every peer
    /// runs Enter/Exit on the matching node with the same payload, late joiners enter the current state.
    /// </summary>
    public class StateMachine : NetworkBehaviour
    {
        [Tooltip("Owner may switch states (otherwise only the server).")] public bool ownerAuth;
        public List<StateNode> states = new List<StateNode>();

        public event Action<StateNode, StateNode> onStateChanged;
        public int currentStateId => _sync.value.id;
        public int previousStateId { get; private set; } = -1;
        public StateNode currentStateNode => currentStateId >= 0 && currentStateId < states.Count ? states[currentStateId] : null;
        public StateNode previousStateNode => previousStateId >= 0 && previousStateId < states.Count ? states[previousStateId] : null;

        public struct State : INetSerializable, IEquatable<State>
        {
            public int id; public uint seq; public byte[] data;
            public void Write(BitWriter w) { w.WriteBits((ulong)(id + 1), 16); w.WriteBits(seq, 32); w.WriteUInt((ulong)(data?.Length ?? 0), 16); if (data != null) w.WriteBytes(data); }
            public void Read(BitReader r) { id = (int)r.ReadBits(16) - 1; seq = (uint)r.ReadBits(32); int n = (int)r.ReadUInt(16); data = n > 0 ? r.ReadBytes(n) : null; }
            public bool Equals(State o) => id == o.id && seq == o.seq;
        }

        readonly SyncVar<State> _sync = new SyncVar<State>(new State { id = -1 });
        int _applied = -1; uint _appliedSeq;

        protected override void OnBeforeBind()
        {
            _sync.ownerAuth = ownerAuth;
            foreach (var s in states) if (s != null) s.machine = this;
            _sync.onChanged += _ => ApplyIncoming();
        }

        protected override void OnSpawned(bool asServer) { ApplyIncoming(); }

        public T GetState<T>() where T : StateNode { foreach (var s in states) if (s is T t) return t; return null; }
        public bool IsCurrentState<T>() where T : StateNode => currentStateNode is T;
        public bool IsCurrentState(StateNode s) => currentStateNode == s;

        /// <summary>Controller: switch to a state without payload.</summary>
        public bool SetState(StateNode state, bool force = false) => Switch(states.IndexOf(state), null, force);
        /// <summary>Controller: switch to a state with a payload.</summary>
        public bool SetState<T>(StateNode<T> state, T data, bool force = false)
        {
            if (!force && !state.CanEnter(data)) return false;
            var w = new BitWriter(); NetSerializer.Write(w, data, typeof(T));
            return Switch(states.IndexOf(state), w.Finish(), force);
        }
        /// <summary>Controller: go to the next state in the list (wraps around).</summary>
        public bool Next(bool force = false) => states.Count == 0 ? false : Switch((currentStateId + 1) % states.Count, null, force);
        public bool Next<T>(T data, bool force = false)
        {
            if (states.Count == 0) return false;
            int n = (currentStateId + 1) % states.Count;
            if (!(states[n] is StateNode<T> typed)) return false;
            return SetState(typed, data, force);
        }
        /// <summary>Controller: leave the current state (no state active).</summary>
        public bool Stop(bool force = false) => Switch(-1, null, force);

        bool Switch(int id, byte[] data, bool force)
        {
            if (!isSpawned || !isController) return false;
            if (id >= states.Count) return false;
            var cur = currentStateNode;
            if (!force && cur != null && !cur.CanExit()) return false;
            if (!force && id >= 0 && !states[id].CanEnter()) return false;
            _sync.value = new State { id = id, seq = _sync.value.seq + 1, data = data };
            ApplyIncoming();
            return true;
        }

        void ApplyIncoming()
        {
            var st = _sync.value;
            if (st.id == _applied && st.seq == _appliedSeq) return;
            bool asServer = isServer;
            var prev = _applied >= 0 && _applied < states.Count ? states[_applied] : null;
            if (prev != null) { prev.Exit(); prev.Exit(asServer); }
            previousStateId = _applied; _applied = st.id; _appliedSeq = st.seq;
            var next = currentStateNode;
            if (next != null) next.EnterWithData(st.data != null ? new BitReader(st.data) : null, asServer);
            onStateChanged?.Invoke(prev, next);
        }

        public override void OnTick(uint tick)
        {
            var cur = currentStateNode;
            if (cur != null) { cur.StateUpdate(); cur.StateUpdate(isServer); }
        }
    }
}
