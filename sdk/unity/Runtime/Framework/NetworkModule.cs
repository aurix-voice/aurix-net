// Synchronised state containers for NetworkBehaviours: SyncVar<T>, SyncList<T>, SyncDictionary<K,V>.
// The server (room host) is authoritative; a module with ownerAuth = true is written by the owner
// instead. Changes are collected once per network tick and sent as deltas; a spawn (or late join)
// carries the full state.
using System;
using System.Collections;
using System.Collections.Generic;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    public abstract class NetworkModule
    {
        public NetworkBehaviour behaviour { get; private set; }
        public NetworkIdentity identity => behaviour?.identity;
        public int index { get; private set; }
        public bool ownerAuth { get; protected set; }

        internal void SetAuthority(bool ownerAuth)
        {
            this.ownerAuth = ownerAuth;
        }

        /// <summary>Owner-auth modules only: send to the owner and the server, not to other observers.</summary>
        public bool ownerOnly { get; protected set; }

        /// <summary>Owner-auth modules only: the server consumes the data and does not relay it to observers (inputs).</summary>
        public bool serverOnly { get; protected set; }
        public float sendIntervalInSeconds { get; set; }
        internal float nextSendTime;
        public bool isDirty { get; protected set; }

        /// <summary>Server tick of the delta being applied (set before ReadDelta/ReadFull).</summary>
        public uint lastReceivedTick { get; internal set; }

        /// <summary>True when the local side may write this module (server, or the owner for ownerAuth modules).</summary>
        public bool isController
        {
            get
            {
                var id = identity;
                if (id == null || !id.isSpawned)
                {
                    return true; // unspawned: local edits are the initial state
                }

                return ownerAuth ? id.IsController(true) : id.isServer;
            }
        }

        internal void Bind(NetworkBehaviour target, int idx)
        {
            behaviour = target;
            index = idx;
        }

        internal void ClearDirty()
        {
            isDirty = false;
        }

        /// <summary>Server side: an owner-authoritative delta arrived. Return false to reject it; the server then sends its own state back to the owner and nothing to observers.</summary>
        public virtual bool ReadDeltaFromOwner(BitReader reader)
        {
            ReadDelta(reader);
            return true;
        }

        /// <summary>Called by the manager each tick before the dirty check (controller side).</summary>
        public virtual void OnBeforeSend() { }

        public void SetDirty()
        {
            if (identity != null && identity.isSpawned && !isController)
            {
                Debug.LogWarning(
                    $"{behaviour.GetType().Name}.{GetType().Name}: only the {(ownerAuth ? "owner" : "server")} may write it"
                );
                return;
            }
            isDirty = true;
        }

        public abstract void WriteFull(BitWriter writer);
        public abstract void ReadFull(BitReader reader);
        public abstract void WriteDelta(BitWriter writer);
        public abstract void ReadDelta(BitReader reader);

        public virtual void OnOwnerChanged(PlayerID? oldOwner, PlayerID? newOwner) { }

        public virtual void OnSpawned(bool asServer) { }

        public virtual void OnDespawned() { }
    }

    public sealed class SyncVar<T> : NetworkModule
    {
        T _value;
        public event Action<T> onChanged;
        public delegate void ActionWithOld(T oldValue, T newValue);
        public event ActionWithOld onChangedWithOld;

        public SyncVar(
            T initialValue = default,
            float sendIntervalInSeconds = 0f,
            bool ownerAuth = false,
            bool ownerOnly = false
        )
        {
            _value = initialValue;
            this.sendIntervalInSeconds = sendIntervalInSeconds;
            this.ownerAuth = ownerAuth;
            this.ownerOnly = ownerOnly;
        }

        public T value
        {
            get => _value;
            set
            {
                if (NetSerializer.AreEqual(_value, value))
                {
                    return;
                }

                if (identity != null && identity.isSpawned && !isController)
                {
                    Debug.LogWarning(
                        $"{behaviour.GetType().Name}.SyncVar<{typeof(T).Name}>: only the {(ownerAuth ? "owner" : "server")} may write it"
                    );
                    return;
                }
                var old = _value;
                _value = value;
                isDirty = true;
                onChanged?.Invoke(value);
                onChangedWithOld?.Invoke(old, value);
            }
        }

        public override void WriteFull(BitWriter writer) =>
            NetSerializer.Write(writer, _value, typeof(T));

        public override void ReadFull(BitReader reader) =>
            Apply((T)NetSerializer.Read(reader, typeof(T)));

        public override void WriteDelta(BitWriter writer) => WriteFull(writer);

        public override void ReadDelta(BitReader reader) => ReadFull(reader);

        void Apply(T value)
        {
            if (NetSerializer.AreEqual(_value, value))
            {
                return;
            }

            var old = _value;
            _value = value;
            onChanged?.Invoke(value);
            onChangedWithOld?.Invoke(old, value);
        }

        public static implicit operator T(SyncVar<T> source) => source._value;

        public override string ToString() => _value?.ToString() ?? "null";
    }

    public enum SyncListOperation : byte
    {
        Added,
        Removed,
        Inserted,
        Set,
        Cleared,
    }

    public readonly struct SyncListChange<T>
    {
        public readonly SyncListOperation operation;
        public readonly T value;
        public readonly T oldValue;
        public readonly int index;

        public SyncListChange(SyncListOperation op, T value, T oldValue, int index)
        {
            operation = op;
            this.value = value;
            this.oldValue = oldValue;
            this.index = index;
        }

        public override string ToString() => $"{operation} [{index}] {value}";
    }

    public sealed class SyncList<T> : NetworkModule, IList<T>, IReadOnlyList<T>
    {
        readonly List<T> _list = new List<T>();
        readonly List<SyncListChange<T>> _pending = new List<SyncListChange<T>>();
        public event Action<SyncListChange<T>> onChanged;

        public SyncList(bool ownerAuth = false, bool ownerOnly = false)
        {
            this.ownerAuth = ownerAuth;
            this.ownerOnly = ownerOnly;
        }

        public int Count => _list.Count;
        public bool IsReadOnly => false;

        public List<T> ToList() => new List<T>(_list);

        public T this[int index]
        {
            get => _list[index];
            set
            {
                if (!Allowed())
                {
                    return;
                }

                var old = _list[index];
                _list[index] = value;
                Record(new SyncListChange<T>(SyncListOperation.Set, value, old, index));
            }
        }

        public void Add(T item)
        {
            if (!Allowed())
            {
                return;
            }

            _list.Add(item);
            Record(new SyncListChange<T>(SyncListOperation.Added, item, default, _list.Count - 1));
        }

        public void Insert(int index, T item)
        {
            if (!Allowed())
            {
                return;
            }

            _list.Insert(index, item);
            Record(new SyncListChange<T>(SyncListOperation.Inserted, item, default, index));
        }

        public bool Remove(T item)
        {
            int index = _list.IndexOf(item);
            if (index < 0)
            {
                return false;
            }

            RemoveAt(index);
            return true;
        }

        public void RemoveAt(int index)
        {
            if (!Allowed())
            {
                return;
            }

            var old = _list[index];
            _list.RemoveAt(index);
            Record(new SyncListChange<T>(SyncListOperation.Removed, default, old, index));
        }

        public void Clear()
        {
            if (!Allowed())
            {
                return;
            }

            _list.Clear();
            Record(new SyncListChange<T>(SyncListOperation.Cleared, default, default, 0));
        }

        public bool Contains(T item) => _list.Contains(item);

        public int IndexOf(T item) => _list.IndexOf(item);

        public void CopyTo(T[] array, int index) => _list.CopyTo(array, index);

        public IEnumerator<T> GetEnumerator() => _list.GetEnumerator();

        IEnumerator IEnumerable.GetEnumerator() => _list.GetEnumerator();

        bool Allowed()
        {
            if (identity == null || !identity.isSpawned || isController)
            {
                return true;
            }

            Debug.LogWarning(
                $"{behaviour.GetType().Name}.SyncList<{typeof(T).Name}>: only the {(ownerAuth ? "owner" : "server")} may write it"
            );
            return false;
        }

        void Record(SyncListChange<T> change)
        {
            if (change.operation == SyncListOperation.Cleared)
            {
                _pending.Clear();
            }

            if (_pending.Count >= 255)
            {
                _pending.Clear();
                _pending.Add(
                    new SyncListChange<T>(SyncListOperation.Cleared, default, default, -1)
                );
            }
            _pending.Add(change);
            isDirty = true;
            onChanged?.Invoke(change);
        }

        public override void WriteFull(BitWriter writer)
        {
            writer.WriteUInt((ulong)_list.Count, 16);
            foreach (var item in _list)
            {
                NetSerializer.Write(writer, item, typeof(T));
            }
        }

        public override void ReadFull(BitReader reader)
        {
            _list.Clear();
            _pending.Clear();
            int count = (int)reader.ReadUInt(16);
            for (int i = 0; i < count; i++)
            {
                _list.Add((T)NetSerializer.Read(reader, typeof(T)));
            }

            onChanged?.Invoke(
                new SyncListChange<T>(SyncListOperation.Cleared, default, default, -1)
            );
        }

        public override void WriteDelta(BitWriter writer)
        {
            // index -1 Cleared = "resend everything" marker after an overflow
            if (
                _pending.Count > 0
                && _pending[0].operation == SyncListOperation.Cleared
                && _pending[0].index == -1
            )
            {
                writer.WriteBits(0, 8);
                WriteFull(writer);
                _pending.Clear();
                return;
            }
            writer.WriteBits((ulong)_pending.Count, 8);
            foreach (var change in _pending)
            {
                writer.WriteBits((byte)change.operation, 3);
                writer.WriteUInt((ulong)Math.Max(0, change.index), 16);
                if (
                    change.operation == SyncListOperation.Added
                    || change.operation == SyncListOperation.Inserted
                    || change.operation == SyncListOperation.Set
                )
                {
                    NetSerializer.Write(writer, change.value, typeof(T));
                }
            }
            _pending.Clear();
        }

        public override void ReadDelta(BitReader reader)
        {
            int count = (int)reader.ReadBits(8);
            if (count == 0)
            {
                ReadFull(reader);
                return;
            }
            for (int k = 0; k < count; k++)
            {
                var op = (SyncListOperation)reader.ReadBits(3);
                int index = (int)reader.ReadUInt(16);
                switch (op)
                {
                    case SyncListOperation.Added:
                    {
                        var value = (T)NetSerializer.Read(reader, typeof(T));
                        _list.Add(value);
                        onChanged?.Invoke(
                            new SyncListChange<T>(op, value, default, _list.Count - 1)
                        );
                        break;
                    }
                    case SyncListOperation.Inserted:
                    {
                        var value = (T)NetSerializer.Read(reader, typeof(T));
                        index = Math.Min(index, _list.Count);
                        _list.Insert(index, value);
                        onChanged?.Invoke(new SyncListChange<T>(op, value, default, index));
                        break;
                    }
                    case SyncListOperation.Set:
                    {
                        var value = (T)NetSerializer.Read(reader, typeof(T));
                        if (index < _list.Count)
                        {
                            var old = _list[index];
                            _list[index] = value;
                            onChanged?.Invoke(new SyncListChange<T>(op, value, old, index));
                        }
                        break;
                    }
                    case SyncListOperation.Removed:
                    {
                        if (index < _list.Count)
                        {
                            var old = _list[index];
                            _list.RemoveAt(index);
                            onChanged?.Invoke(new SyncListChange<T>(op, default, old, index));
                        }
                        break;
                    }
                    case SyncListOperation.Cleared:
                        _list.Clear();
                        onChanged?.Invoke(new SyncListChange<T>(op, default, default, 0));
                        break;
                }
            }
        }
    }

    public enum SyncDictionaryOperation : byte
    {
        Set,
        Removed,
        Cleared,
    }

    public readonly struct SyncDictionaryChange<K, V>
    {
        public readonly SyncDictionaryOperation operation;
        public readonly K key;
        public readonly V value;

        public SyncDictionaryChange(SyncDictionaryOperation op, K key, V value)
        {
            operation = op;
            this.key = key;
            this.value = value;
        }
    }

    public sealed class SyncDictionary<K, V> : NetworkModule, IEnumerable<KeyValuePair<K, V>>
    {
        readonly Dictionary<K, V> _dict = new Dictionary<K, V>();
        readonly List<SyncDictionaryChange<K, V>> _pending = new List<SyncDictionaryChange<K, V>>();
        public event Action<SyncDictionaryChange<K, V>> onChanged;

        public SyncDictionary(bool ownerAuth = false, bool ownerOnly = false)
        {
            this.ownerAuth = ownerAuth;
            this.ownerOnly = ownerOnly;
        }

        public int Count => _dict.Count;
        public IEnumerable<K> Keys => _dict.Keys;
        public IEnumerable<V> Values => _dict.Values;

        public bool ContainsKey(K key) => _dict.ContainsKey(key);

        public bool TryGetValue(K key, out V value) => _dict.TryGetValue(key, out value);

        public V this[K key]
        {
            get => _dict[key];
            set => Set(key, value);
        }

        public void Add(K key, V value) => Set(key, value);

        public void Set(K key, V value)
        {
            if (!Allowed())
            {
                return;
            }

            _dict[key] = value;
            Record(new SyncDictionaryChange<K, V>(SyncDictionaryOperation.Set, key, value));
        }

        public bool Remove(K key)
        {
            if (!Allowed() || !_dict.Remove(key))
            {
                return false;
            }

            Record(new SyncDictionaryChange<K, V>(SyncDictionaryOperation.Removed, key, default));
            return true;
        }

        public void Clear()
        {
            if (!Allowed())
            {
                return;
            }

            _dict.Clear();
            _pending.Clear();
            Record(
                new SyncDictionaryChange<K, V>(SyncDictionaryOperation.Cleared, default, default)
            );
        }

        public IEnumerator<KeyValuePair<K, V>> GetEnumerator() => _dict.GetEnumerator();

        IEnumerator IEnumerable.GetEnumerator() => _dict.GetEnumerator();

        bool Allowed()
        {
            if (identity == null || !identity.isSpawned || isController)
            {
                return true;
            }

            Debug.LogWarning(
                $"{behaviour.GetType().Name}.SyncDictionary: only the {(ownerAuth ? "owner" : "server")} may write it"
            );
            return false;
        }

        void Record(SyncDictionaryChange<K, V> change)
        {
            if (_pending.Count >= 255)
            {
                _pending.Clear();
                _pending.Add(
                    new SyncDictionaryChange<K, V>(
                        SyncDictionaryOperation.Cleared,
                        default,
                        default
                    )
                );
                foreach (var kv in _dict)
                {
                    _pending.Add(
                        new SyncDictionaryChange<K, V>(
                            SyncDictionaryOperation.Set,
                            kv.Key,
                            kv.Value
                        )
                    );
                }
            }
            else
            {
                _pending.Add(change);
            }

            isDirty = true;
            onChanged?.Invoke(change);
        }

        public override void WriteFull(BitWriter writer)
        {
            writer.WriteUInt((ulong)_dict.Count, 16);
            foreach (var kv in _dict)
            {
                NetSerializer.Write(writer, kv.Key, typeof(K));
                NetSerializer.Write(writer, kv.Value, typeof(V));
            }
        }

        public override void ReadFull(BitReader reader)
        {
            _dict.Clear();
            _pending.Clear();
            int count = (int)reader.ReadUInt(16);
            for (int i = 0; i < count; i++)
            {
                var key = (K)NetSerializer.Read(reader, typeof(K));
                _dict[key] = (V)NetSerializer.Read(reader, typeof(V));
            }
            onChanged?.Invoke(
                new SyncDictionaryChange<K, V>(SyncDictionaryOperation.Cleared, default, default)
            );
        }

        public override void WriteDelta(BitWriter writer)
        {
            writer.WriteBits((ulong)_pending.Count, 9);
            foreach (var change in _pending)
            {
                writer.WriteBits((byte)change.operation, 2);
                if (change.operation != SyncDictionaryOperation.Cleared)
                {
                    NetSerializer.Write(writer, change.key, typeof(K));
                }

                if (change.operation == SyncDictionaryOperation.Set)
                {
                    NetSerializer.Write(writer, change.value, typeof(V));
                }
            }
            _pending.Clear();
        }

        public override void ReadDelta(BitReader reader)
        {
            int count = (int)reader.ReadBits(9);
            for (int i = 0; i < count; i++)
            {
                var op = (SyncDictionaryOperation)reader.ReadBits(2);
                switch (op)
                {
                    case SyncDictionaryOperation.Set:
                    {
                        var key = (K)NetSerializer.Read(reader, typeof(K));
                        var value = (V)NetSerializer.Read(reader, typeof(V));
                        _dict[key] = value;
                        onChanged?.Invoke(new SyncDictionaryChange<K, V>(op, key, value));
                        break;
                    }
                    case SyncDictionaryOperation.Removed:
                    {
                        var key = (K)NetSerializer.Read(reader, typeof(K));
                        _dict.Remove(key);
                        onChanged?.Invoke(new SyncDictionaryChange<K, V>(op, key, default));
                        break;
                    }
                    case SyncDictionaryOperation.Cleared:
                        _dict.Clear();
                        onChanged?.Invoke(new SyncDictionaryChange<K, V>(op, default, default));
                        break;
                }
            }
        }
    }
}
