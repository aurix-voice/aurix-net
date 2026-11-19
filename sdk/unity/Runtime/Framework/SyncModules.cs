// More sync modules in the SyncList family: SyncHashSet, SyncQueue, SyncArray, SyncEvent, SyncTimer.
// Same rules: server- or owner-authoritative, full state on spawn/late join, per-tick deltas, onChanged.
using System;
using System.Collections;
using System.Collections.Generic;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    public enum SyncSetOperation : byte
    {
        Added,
        Removed,
        Cleared,
    }

    public sealed class SyncHashSet<T> : NetworkModule, IReadOnlyCollection<T>
    {
        readonly HashSet<T> _set = new HashSet<T>();
        readonly List<(SyncSetOperation op, T item)> _pending = new List<(SyncSetOperation, T)>();
        public event Action<SyncSetOperation, T> onChanged;

        public SyncHashSet(bool ownerAuth = false, bool ownerOnly = false)
        {
            this.ownerAuth = ownerAuth;
            this.ownerOnly = ownerOnly;
        }

        public int Count => _set.Count;

        public bool Contains(T item) => _set.Contains(item);

        public bool Add(T item)
        {
            if (!Allowed() || !_set.Add(item))
            {
                return false;
            }

            Record(SyncSetOperation.Added, item);
            return true;
        }

        public bool Remove(T item)
        {
            if (!Allowed() || !_set.Remove(item))
            {
                return false;
            }

            Record(SyncSetOperation.Removed, item);
            return true;
        }

        public void Clear()
        {
            if (!Allowed())
            {
                return;
            }

            _set.Clear();
            _pending.Clear();
            Record(SyncSetOperation.Cleared, default);
        }

        public IEnumerator<T> GetEnumerator() => _set.GetEnumerator();

        IEnumerator IEnumerable.GetEnumerator() => _set.GetEnumerator();

        bool Allowed()
        {
            if (identity == null || !identity.isSpawned || isController)
            {
                return true;
            }

            Debug.LogWarning(
                $"{behaviour.GetType().Name}.SyncHashSet<{typeof(T).Name}>: only the {(ownerAuth ? "owner" : "server")} may write it"
            );
            return false;
        }

        void Record(SyncSetOperation op, T item)
        {
            if (_pending.Count >= 255)
            {
                _pending.Clear();
                _pending.Add((SyncSetOperation.Cleared, default));
                foreach (var element in _set)
                {
                    _pending.Add((SyncSetOperation.Added, element));
                }
            }
            else
            {
                _pending.Add((op, item));
            }

            isDirty = true;
            onChanged?.Invoke(op, item);
        }

        public override void WriteFull(BitWriter writer)
        {
            writer.WriteUInt((ulong)_set.Count, 16);
            foreach (var item in _set)
            {
                NetSerializer.Write(writer, item, typeof(T));
            }
        }

        public override void ReadFull(BitReader reader)
        {
            _set.Clear();
            _pending.Clear();
            int count = (int)reader.ReadUInt(16);
            for (int i = 0; i < count; i++)
            {
                _set.Add((T)NetSerializer.Read(reader, typeof(T)));
            }

            onChanged?.Invoke(SyncSetOperation.Cleared, default);
        }

        public override void WriteDelta(BitWriter writer)
        {
            writer.WriteBits((ulong)_pending.Count, 8);
            foreach (var (op, item) in _pending)
            {
                writer.WriteBits((ulong)op, 2);
                if (op != SyncSetOperation.Cleared)
                {
                    NetSerializer.Write(writer, item, typeof(T));
                }
            }
            _pending.Clear();
        }

        public override void ReadDelta(BitReader reader)
        {
            int count = (int)reader.ReadBits(8);
            for (int i = 0; i < count; i++)
            {
                var op = (SyncSetOperation)reader.ReadBits(2);
                T item = default;
                if (op != SyncSetOperation.Cleared)
                {
                    item = (T)NetSerializer.Read(reader, typeof(T));
                }

                if (op == SyncSetOperation.Added)
                {
                    _set.Add(item);
                }
                else if (op == SyncSetOperation.Removed)
                {
                    _set.Remove(item);
                }
                else
                {
                    _set.Clear();
                }

                onChanged?.Invoke(op, item);
            }
        }
    }

    public enum SyncQueueOperation : byte
    {
        Enqueued,
        Dequeued,
        Cleared,
    }

    public sealed class SyncQueue<T> : NetworkModule, IReadOnlyCollection<T>
    {
        readonly Queue<T> _q = new Queue<T>();
        readonly List<(SyncQueueOperation op, T item)> _pending =
            new List<(SyncQueueOperation, T)>();
        public event Action<SyncQueueOperation, T> onChanged;

        public SyncQueue(bool ownerAuth = false, bool ownerOnly = false)
        {
            this.ownerAuth = ownerAuth;
            this.ownerOnly = ownerOnly;
        }

        public int Count => _q.Count;

        public T Peek() => _q.Peek();

        public bool TryPeek(out T item)
        {
            if (_q.Count == 0)
            {
                item = default;
                return false;
            }
            item = _q.Peek();
            return true;
        }

        public void Enqueue(T item)
        {
            if (!Allowed())
            {
                return;
            }

            _q.Enqueue(item);
            Record(SyncQueueOperation.Enqueued, item);
        }

        public T Dequeue()
        {
            if (!Allowed())
            {
                throw new InvalidOperationException("not the controller");
            }

            var item = _q.Dequeue();
            Record(SyncQueueOperation.Dequeued, item);
            return item;
        }

        public bool TryDequeue(out T item)
        {
            if (_q.Count == 0 || !Allowed())
            {
                item = default;
                return false;
            }
            item = Dequeue();
            return true;
        }

        public void Clear()
        {
            if (!Allowed())
            {
                return;
            }

            _q.Clear();
            _pending.Clear();
            Record(SyncQueueOperation.Cleared, default);
        }

        public IEnumerator<T> GetEnumerator() => _q.GetEnumerator();

        IEnumerator IEnumerable.GetEnumerator() => _q.GetEnumerator();

        bool Allowed()
        {
            if (identity == null || !identity.isSpawned || isController)
            {
                return true;
            }

            Debug.LogWarning(
                $"{behaviour.GetType().Name}.SyncQueue<{typeof(T).Name}>: only the {(ownerAuth ? "owner" : "server")} may write it"
            );
            return false;
        }

        void Record(SyncQueueOperation op, T item)
        {
            if (_pending.Count >= 255)
            {
                _pending.Clear();
                _pending.Add((SyncQueueOperation.Cleared, default));
                foreach (var element in _q)
                {
                    _pending.Add((SyncQueueOperation.Enqueued, element));
                }
            }
            else
            {
                _pending.Add((op, item));
            }

            isDirty = true;
            onChanged?.Invoke(op, item);
        }

        public override void WriteFull(BitWriter writer)
        {
            writer.WriteUInt((ulong)_q.Count, 16);
            foreach (var item in _q)
            {
                NetSerializer.Write(writer, item, typeof(T));
            }
        }

        public override void ReadFull(BitReader reader)
        {
            _q.Clear();
            _pending.Clear();
            int count = (int)reader.ReadUInt(16);
            for (int i = 0; i < count; i++)
            {
                _q.Enqueue((T)NetSerializer.Read(reader, typeof(T)));
            }

            onChanged?.Invoke(SyncQueueOperation.Cleared, default);
        }

        public override void WriteDelta(BitWriter writer)
        {
            writer.WriteBits((ulong)_pending.Count, 8);
            foreach (var (op, item) in _pending)
            {
                writer.WriteBits((ulong)op, 2);
                if (op == SyncQueueOperation.Enqueued)
                {
                    NetSerializer.Write(writer, item, typeof(T));
                }
            }
            _pending.Clear();
        }

        public override void ReadDelta(BitReader reader)
        {
            int count = (int)reader.ReadBits(8);
            for (int i = 0; i < count; i++)
            {
                var op = (SyncQueueOperation)reader.ReadBits(2);
                T item = default;
                if (op == SyncQueueOperation.Enqueued)
                {
                    item = (T)NetSerializer.Read(reader, typeof(T));
                    _q.Enqueue(item);
                }
                else if (op == SyncQueueOperation.Dequeued)
                {
                    if (_q.Count > 0)
                    {
                        item = _q.Dequeue();
                    }
                }
                else
                {
                    _q.Clear();
                }

                onChanged?.Invoke(op, item);
            }
        }
    }

    /// <summary>Fixed-length array; only changed slots travel.</summary>
    public sealed class SyncArray<T> : NetworkModule, IReadOnlyList<T>
    {
        readonly T[] _a;
        readonly HashSet<int> _dirtySlots = new HashSet<int>();
        public event Action<int, T> onChanged;

        public SyncArray(int length, bool ownerAuth = false, bool ownerOnly = false)
        {
            _a = new T[length];
            this.ownerAuth = ownerAuth;
            this.ownerOnly = ownerOnly;
        }

        public int Count => _a.Length;
        public int Length => _a.Length;
        public T this[int index]
        {
            get => _a[index];
            set
            {
                if (identity != null && identity.isSpawned && !isController)
                {
                    Debug.LogWarning(
                        $"{behaviour.GetType().Name}.SyncArray<{typeof(T).Name}>: only the {(ownerAuth ? "owner" : "server")} may write it"
                    );
                    return;
                }
                _a[index] = value;
                _dirtySlots.Add(index);
                isDirty = true;
                onChanged?.Invoke(index, value);
            }
        }

        public IEnumerator<T> GetEnumerator() => ((IEnumerable<T>)_a).GetEnumerator();

        IEnumerator IEnumerable.GetEnumerator() => _a.GetEnumerator();

        public override void WriteFull(BitWriter writer)
        {
            foreach (var item in _a)
            {
                NetSerializer.Write(writer, item, typeof(T));
            }
        }

        public override void ReadFull(BitReader reader)
        {
            for (int i = 0; i < _a.Length; i++)
            {
                _a[i] = (T)NetSerializer.Read(reader, typeof(T));
                onChanged?.Invoke(i, _a[i]);
            }
        }

        public override void WriteDelta(BitWriter writer)
        {
            writer.WriteBits((ulong)_dirtySlots.Count, 16);
            foreach (var index in _dirtySlots)
            {
                writer.WriteBits((ulong)index, 16);
                NetSerializer.Write(writer, _a[index], typeof(T));
            }
            _dirtySlots.Clear();
        }

        public override void ReadDelta(BitReader reader)
        {
            int count = (int)reader.ReadBits(16);
            for (int k = 0; k < count; k++)
            {
                int index = (int)reader.ReadBits(16);
                _a[index] = (T)NetSerializer.Read(reader, typeof(T));
                onChanged?.Invoke(index, _a[index]);
            }
        }
    }

    /// <summary>Fire-and-forget event with a payload; late joiners do not receive past events (unlike bufferLast RPCs).</summary>
    public sealed class SyncEvent<T> : NetworkModule
    {
        readonly List<T> _pending = new List<T>();
        public event Action<T> onInvoked;

        public SyncEvent(bool ownerAuth = false)
        {
            this.ownerAuth = ownerAuth;
        }

        public void Invoke(T arg)
        {
            if (identity != null && identity.isSpawned && !isController)
            {
                Debug.LogWarning(
                    $"{behaviour.GetType().Name}.SyncEvent<{typeof(T).Name}>: only the {(ownerAuth ? "owner" : "server")} may invoke it"
                );
                return;
            }
            _pending.Add(arg);
            isDirty = true;
            onInvoked?.Invoke(arg);
        }

        public override void WriteFull(BitWriter writer) { }

        public override void ReadFull(BitReader reader) { }

        public override void WriteDelta(BitWriter writer)
        {
            writer.WriteBits((ulong)_pending.Count, 8);
            foreach (var item in _pending)
            {
                NetSerializer.Write(writer, item, typeof(T));
            }

            _pending.Clear();
        }

        public override void ReadDelta(BitReader reader)
        {
            int count = (int)reader.ReadBits(8);
            for (int i = 0; i < count; i++)
            {
                onInvoked?.Invoke((T)NetSerializer.Read(reader, typeof(T)));
            }
        }
    }

    /// <summary>Server-driven countdown replicated by tick: clients compute the remaining time locally from the server tick.</summary>
    public sealed class SyncTimer : NetworkModule
    {
        uint _endTick;
        bool _running;
        bool _changed;
        public event Action onFinished,
            onStarted,
            onStopped;
        bool _finishedFired;

        public SyncTimer(bool ownerAuth = false)
        {
            this.ownerAuth = ownerAuth;
        }

        public bool isRunning => _running && !finished;
        public uint endTick => _endTick;
        public bool finished => _running && Tick() >= _endTick;
        public float remaining =>
            !_running
                ? 0f
                : Math.Max(0, (int)(_endTick - Tick()))
                    / (float)Math.Max(1, behaviour?.networkManager?.tickRate ?? 60);

        uint Tick() => behaviour?.networkManager?.serverTick ?? 0;

        public void Start(float seconds)
        {
            if (identity != null && identity.isSpawned && !isController)
            {
                Debug.LogWarning(
                    $"{behaviour.GetType().Name}.SyncTimer: only the {(ownerAuth ? "owner" : "server")} may start it"
                );
                return;
            }
            _endTick =
                Tick()
                + (uint)
                    Math.Max(
                        1,
                        Math.Round(seconds * Math.Max(1, behaviour?.networkManager?.tickRate ?? 60))
                    );
            _running = true;
            _finishedFired = false;
            _changed = true;
            isDirty = true;
            onStarted?.Invoke();
        }

        public void Stop()
        {
            if (identity != null && identity.isSpawned && !isController)
            {
                return;
            }

            _running = false;
            _changed = true;
            isDirty = true;
            onStopped?.Invoke();
        }

        /// <summary>Call from Update/OnTick to fire onFinished once.</summary>
        public void Poll()
        {
            if (_running && !_finishedFired && Tick() >= _endTick)
            {
                _finishedFired = true;
                onFinished?.Invoke();
            }
        }

        public override void WriteFull(BitWriter writer)
        {
            writer.WriteBool(_running);
            writer.WriteBits(_endTick, 32);
        }

        public override void ReadFull(BitReader reader)
        {
            bool was = _running;
            _running = reader.ReadBool();
            _endTick = (uint)reader.ReadBits(32);
            _finishedFired = false;
            if (_running && !was)
            {
                onStarted?.Invoke();
            }

            if (!_running && was)
            {
                onStopped?.Invoke();
            }
        }

        public override void WriteDelta(BitWriter writer)
        {
            WriteFull(writer);
            _changed = false;
        }

        public override void ReadDelta(BitReader reader) => ReadFull(reader);
    }
}
