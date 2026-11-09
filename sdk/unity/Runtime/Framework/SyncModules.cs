// More sync modules in the SyncList family: SyncHashSet, SyncQueue, SyncArray, SyncEvent, SyncTimer.
// Same rules: server- or owner-authoritative, full state on spawn/late join, per-tick deltas, onChanged.
using System;
using System.Collections;
using System.Collections.Generic;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    public enum SyncSetOperation : byte { Added, Removed, Cleared }

    public sealed class SyncHashSet<T> : NetworkModule, IReadOnlyCollection<T>
    {
        readonly HashSet<T> _set = new HashSet<T>();
        readonly List<(SyncSetOperation op, T item)> _pending = new List<(SyncSetOperation, T)>();
        public event Action<SyncSetOperation, T> onChanged;
        public SyncHashSet(bool ownerAuth = false, bool ownerOnly = false) { this.ownerAuth = ownerAuth; this.ownerOnly = ownerOnly; }
        public int Count => _set.Count;
        public bool Contains(T x) => _set.Contains(x);
        public bool Add(T x) { if (!Allowed() || !_set.Add(x)) return false; Record(SyncSetOperation.Added, x); return true; }
        public bool Remove(T x) { if (!Allowed() || !_set.Remove(x)) return false; Record(SyncSetOperation.Removed, x); return true; }
        public void Clear() { if (!Allowed()) return; _set.Clear(); _pending.Clear(); Record(SyncSetOperation.Cleared, default); }
        public IEnumerator<T> GetEnumerator() => _set.GetEnumerator();
        IEnumerator IEnumerable.GetEnumerator() => _set.GetEnumerator();
        bool Allowed() { if (identity == null || !identity.isSpawned || isController) return true; Debug.LogWarning($"{behaviour.GetType().Name}.SyncHashSet<{typeof(T).Name}>: only the {(ownerAuth ? "owner" : "server")} may write it"); return false; }
        void Record(SyncSetOperation op, T x) { if (_pending.Count >= 255) { _pending.Clear(); _pending.Add((SyncSetOperation.Cleared, default)); foreach (var e in _set) _pending.Add((SyncSetOperation.Added, e)); } else _pending.Add((op, x)); isDirty = true; onChanged?.Invoke(op, x); }
        public override void WriteFull(BitWriter w) { w.WriteUInt((ulong)_set.Count, 16); foreach (var x in _set) NetSerializer.Write(w, x, typeof(T)); }
        public override void ReadFull(BitReader r) { _set.Clear(); _pending.Clear(); int n = (int)r.ReadUInt(16); for (int i = 0; i < n; i++) _set.Add((T)NetSerializer.Read(r, typeof(T))); onChanged?.Invoke(SyncSetOperation.Cleared, default); }
        public override void WriteDelta(BitWriter w) { w.WriteBits((ulong)_pending.Count, 8); foreach (var (op, x) in _pending) { w.WriteBits((ulong)op, 2); if (op != SyncSetOperation.Cleared) NetSerializer.Write(w, x, typeof(T)); } _pending.Clear(); }
        public override void ReadDelta(BitReader r)
        {
            int n = (int)r.ReadBits(8);
            for (int i = 0; i < n; i++)
            {
                var op = (SyncSetOperation)r.ReadBits(2); T x = default;
                if (op != SyncSetOperation.Cleared) x = (T)NetSerializer.Read(r, typeof(T));
                if (op == SyncSetOperation.Added) _set.Add(x); else if (op == SyncSetOperation.Removed) _set.Remove(x); else _set.Clear();
                onChanged?.Invoke(op, x);
            }
        }
    }

    public enum SyncQueueOperation : byte { Enqueued, Dequeued, Cleared }

    public sealed class SyncQueue<T> : NetworkModule, IReadOnlyCollection<T>
    {
        readonly Queue<T> _q = new Queue<T>();
        readonly List<(SyncQueueOperation op, T item)> _pending = new List<(SyncQueueOperation, T)>();
        public event Action<SyncQueueOperation, T> onChanged;
        public SyncQueue(bool ownerAuth = false, bool ownerOnly = false) { this.ownerAuth = ownerAuth; this.ownerOnly = ownerOnly; }
        public int Count => _q.Count;
        public T Peek() => _q.Peek();
        public bool TryPeek(out T x) { if (_q.Count == 0) { x = default; return false; } x = _q.Peek(); return true; }
        public void Enqueue(T x) { if (!Allowed()) return; _q.Enqueue(x); Record(SyncQueueOperation.Enqueued, x); }
        public T Dequeue() { if (!Allowed()) throw new InvalidOperationException("not the controller"); var x = _q.Dequeue(); Record(SyncQueueOperation.Dequeued, x); return x; }
        public bool TryDequeue(out T x) { if (_q.Count == 0 || !Allowed()) { x = default; return false; } x = Dequeue(); return true; }
        public void Clear() { if (!Allowed()) return; _q.Clear(); _pending.Clear(); Record(SyncQueueOperation.Cleared, default); }
        public IEnumerator<T> GetEnumerator() => _q.GetEnumerator();
        IEnumerator IEnumerable.GetEnumerator() => _q.GetEnumerator();
        bool Allowed() { if (identity == null || !identity.isSpawned || isController) return true; Debug.LogWarning($"{behaviour.GetType().Name}.SyncQueue<{typeof(T).Name}>: only the {(ownerAuth ? "owner" : "server")} may write it"); return false; }
        void Record(SyncQueueOperation op, T x) { if (_pending.Count >= 255) { _pending.Clear(); _pending.Add((SyncQueueOperation.Cleared, default)); foreach (var e in _q) _pending.Add((SyncQueueOperation.Enqueued, e)); } else _pending.Add((op, x)); isDirty = true; onChanged?.Invoke(op, x); }
        public override void WriteFull(BitWriter w) { w.WriteUInt((ulong)_q.Count, 16); foreach (var x in _q) NetSerializer.Write(w, x, typeof(T)); }
        public override void ReadFull(BitReader r) { _q.Clear(); _pending.Clear(); int n = (int)r.ReadUInt(16); for (int i = 0; i < n; i++) _q.Enqueue((T)NetSerializer.Read(r, typeof(T))); onChanged?.Invoke(SyncQueueOperation.Cleared, default); }
        public override void WriteDelta(BitWriter w) { w.WriteBits((ulong)_pending.Count, 8); foreach (var (op, x) in _pending) { w.WriteBits((ulong)op, 2); if (op == SyncQueueOperation.Enqueued) NetSerializer.Write(w, x, typeof(T)); } _pending.Clear(); }
        public override void ReadDelta(BitReader r)
        {
            int n = (int)r.ReadBits(8);
            for (int i = 0; i < n; i++)
            {
                var op = (SyncQueueOperation)r.ReadBits(2); T x = default;
                if (op == SyncQueueOperation.Enqueued) { x = (T)NetSerializer.Read(r, typeof(T)); _q.Enqueue(x); }
                else if (op == SyncQueueOperation.Dequeued) { if (_q.Count > 0) x = _q.Dequeue(); }
                else _q.Clear();
                onChanged?.Invoke(op, x);
            }
        }
    }

    /// <summary>Fixed-length array; only changed slots travel.</summary>
    public sealed class SyncArray<T> : NetworkModule, IReadOnlyList<T>
    {
        readonly T[] _a; readonly HashSet<int> _dirtySlots = new HashSet<int>();
        public event Action<int, T> onChanged;
        public SyncArray(int length, bool ownerAuth = false, bool ownerOnly = false) { _a = new T[length]; this.ownerAuth = ownerAuth; this.ownerOnly = ownerOnly; }
        public int Count => _a.Length;
        public int Length => _a.Length;
        public T this[int i]
        {
            get => _a[i];
            set { if (identity != null && identity.isSpawned && !isController) { Debug.LogWarning($"{behaviour.GetType().Name}.SyncArray<{typeof(T).Name}>: only the {(ownerAuth ? "owner" : "server")} may write it"); return; } _a[i] = value; _dirtySlots.Add(i); isDirty = true; onChanged?.Invoke(i, value); }
        }
        public IEnumerator<T> GetEnumerator() => ((IEnumerable<T>)_a).GetEnumerator();
        IEnumerator IEnumerable.GetEnumerator() => _a.GetEnumerator();
        public override void WriteFull(BitWriter w) { foreach (var x in _a) NetSerializer.Write(w, x, typeof(T)); }
        public override void ReadFull(BitReader r) { for (int i = 0; i < _a.Length; i++) { _a[i] = (T)NetSerializer.Read(r, typeof(T)); onChanged?.Invoke(i, _a[i]); } }
        public override void WriteDelta(BitWriter w) { w.WriteBits((ulong)_dirtySlots.Count, 16); foreach (var i in _dirtySlots) { w.WriteBits((ulong)i, 16); NetSerializer.Write(w, _a[i], typeof(T)); } _dirtySlots.Clear(); }
        public override void ReadDelta(BitReader r) { int n = (int)r.ReadBits(16); for (int k = 0; k < n; k++) { int i = (int)r.ReadBits(16); _a[i] = (T)NetSerializer.Read(r, typeof(T)); onChanged?.Invoke(i, _a[i]); } }
    }

    /// <summary>Fire-and-forget event with a payload; late joiners do not receive past events (unlike bufferLast RPCs).</summary>
    public sealed class SyncEvent<T> : NetworkModule
    {
        readonly List<T> _pending = new List<T>();
        public event Action<T> onInvoked;
        public SyncEvent(bool ownerAuth = false) { this.ownerAuth = ownerAuth; }
        public void Invoke(T arg)
        {
            if (identity != null && identity.isSpawned && !isController) { Debug.LogWarning($"{behaviour.GetType().Name}.SyncEvent<{typeof(T).Name}>: only the {(ownerAuth ? "owner" : "server")} may invoke it"); return; }
            _pending.Add(arg); isDirty = true; onInvoked?.Invoke(arg);
        }
        public override void WriteFull(BitWriter w) { }
        public override void ReadFull(BitReader r) { }
        public override void WriteDelta(BitWriter w) { w.WriteBits((ulong)_pending.Count, 8); foreach (var x in _pending) NetSerializer.Write(w, x, typeof(T)); _pending.Clear(); }
        public override void ReadDelta(BitReader r) { int n = (int)r.ReadBits(8); for (int i = 0; i < n; i++) onInvoked?.Invoke((T)NetSerializer.Read(r, typeof(T))); }
    }

    /// <summary>Server-driven countdown replicated by tick: clients compute the remaining time locally from the server tick.</summary>
    public sealed class SyncTimer : NetworkModule
    {
        uint _endTick; bool _running; bool _changed;
        public event Action onFinished, onStarted, onStopped;
        bool _finishedFired;
        public SyncTimer(bool ownerAuth = false) { this.ownerAuth = ownerAuth; }
        public bool isRunning => _running && !finished;
        public uint endTick => _endTick;
        public bool finished => _running && Tick() >= _endTick;
        public float remaining => !_running ? 0f : Math.Max(0, (int)(_endTick - Tick())) / (float)Math.Max(1, behaviour?.networkManager?.tickRate ?? 60);
        uint Tick() => behaviour?.networkManager?.serverTick ?? 0;
        public void Start(float seconds)
        {
            if (identity != null && identity.isSpawned && !isController) { Debug.LogWarning($"{behaviour.GetType().Name}.SyncTimer: only the {(ownerAuth ? "owner" : "server")} may start it"); return; }
            _endTick = Tick() + (uint)Math.Max(1, Math.Round(seconds * Math.Max(1, behaviour?.networkManager?.tickRate ?? 60))); _running = true; _finishedFired = false; _changed = true; isDirty = true; onStarted?.Invoke();
        }
        public void Stop()
        {
            if (identity != null && identity.isSpawned && !isController) return;
            _running = false; _changed = true; isDirty = true; onStopped?.Invoke();
        }
        /// <summary>Call from Update/OnTick to fire onFinished once.</summary>
        public void Poll() { if (_running && !_finishedFired && Tick() >= _endTick) { _finishedFired = true; onFinished?.Invoke(); } }
        public override void WriteFull(BitWriter w) { w.WriteBool(_running); w.WriteBits(_endTick, 32); }
        public override void ReadFull(BitReader r) { bool was = _running; _running = r.ReadBool(); _endTick = (uint)r.ReadBits(32); _finishedFired = false; if (_running && !was) onStarted?.Invoke(); if (!_running && was) onStopped?.Invoke(); }
        public override void WriteDelta(BitWriter w) { WriteFull(w); _changed = false; }
        public override void ReadDelta(BitReader r) => ReadFull(r);
    }
}
