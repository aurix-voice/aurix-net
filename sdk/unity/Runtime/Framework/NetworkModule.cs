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
        /// <summary>Owner-auth modules only: send to the owner and the server, not to other observers.</summary>
        public bool ownerOnly { get; protected set; }
        public float sendIntervalInSeconds { get; set; }
        internal float nextSendTime;
        public bool isDirty { get; protected set; }

        /// <summary>True when the local side may write this module (server, or the owner for ownerAuth modules).</summary>
        public bool isController
        {
            get
            {
                var id = identity;
                if (id == null || !id.isSpawned) return true; // unspawned: local edits are the initial state
                return ownerAuth ? id.IsController(true) : id.isServer;
            }
        }

        internal void Bind(NetworkBehaviour b, int idx) { behaviour = b; index = idx; }
        internal void ClearDirty() { isDirty = false; }

        public void SetDirty()
        {
            if (identity != null && identity.isSpawned && !isController) { Debug.LogWarning($"{behaviour.GetType().Name}.{GetType().Name}: only the {(ownerAuth ? "owner" : "server")} may write it"); return; }
            isDirty = true;
        }

        public abstract void WriteFull(BitWriter w);
        public abstract void ReadFull(BitReader r);
        public abstract void WriteDelta(BitWriter w);
        public abstract void ReadDelta(BitReader r);
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

        public SyncVar(T initialValue = default, float sendIntervalInSeconds = 0f, bool ownerAuth = false, bool ownerOnly = false)
        {
            _value = initialValue; this.sendIntervalInSeconds = sendIntervalInSeconds; this.ownerAuth = ownerAuth; this.ownerOnly = ownerOnly;
        }

        public T value
        {
            get => _value;
            set
            {
                if (NetSerializer.AreEqual(_value, value)) return;
                if (identity != null && identity.isSpawned && !isController) { Debug.LogWarning($"{behaviour.GetType().Name}.SyncVar<{typeof(T).Name}>: only the {(ownerAuth ? "owner" : "server")} may write it"); return; }
                var old = _value; _value = value; isDirty = true;
                onChanged?.Invoke(value); onChangedWithOld?.Invoke(old, value);
            }
        }

        public override void WriteFull(BitWriter w) => NetSerializer.Write(w, _value, typeof(T));
        public override void ReadFull(BitReader r) => Apply((T)NetSerializer.Read(r, typeof(T)));
        public override void WriteDelta(BitWriter w) => WriteFull(w);
        public override void ReadDelta(BitReader r) => ReadFull(r);

        void Apply(T v)
        {
            if (NetSerializer.AreEqual(_value, v)) return;
            var old = _value; _value = v;
            onChanged?.Invoke(v); onChangedWithOld?.Invoke(old, v);
        }

        public static implicit operator T(SyncVar<T> s) => s._value;
        public override string ToString() => _value?.ToString() ?? "null";
    }

    public enum SyncListOperation : byte { Added, Removed, Inserted, Set, Cleared }

    public readonly struct SyncListChange<T>
    {
        public readonly SyncListOperation operation; public readonly T value; public readonly T oldValue; public readonly int index;
        public SyncListChange(SyncListOperation op, T value, T oldValue, int index) { operation = op; this.value = value; this.oldValue = oldValue; this.index = index; }
        public override string ToString() => $"{operation} [{index}] {value}";
    }

    public sealed class SyncList<T> : NetworkModule, IList<T>, IReadOnlyList<T>
    {
        readonly List<T> _list = new List<T>();
        readonly List<SyncListChange<T>> _pending = new List<SyncListChange<T>>();
        public event Action<SyncListChange<T>> onChanged;

        public SyncList(bool ownerAuth = false, bool ownerOnly = false) { this.ownerAuth = ownerAuth; this.ownerOnly = ownerOnly; }

        public int Count => _list.Count;
        public bool IsReadOnly => false;
        public List<T> ToList() => new List<T>(_list);

        public T this[int i]
        {
            get => _list[i];
            set { if (!Allowed()) return; var old = _list[i]; _list[i] = value; Record(new SyncListChange<T>(SyncListOperation.Set, value, old, i)); }
        }

        public void Add(T item) { if (!Allowed()) return; _list.Add(item); Record(new SyncListChange<T>(SyncListOperation.Added, item, default, _list.Count - 1)); }
        public void Insert(int i, T item) { if (!Allowed()) return; _list.Insert(i, item); Record(new SyncListChange<T>(SyncListOperation.Inserted, item, default, i)); }
        public bool Remove(T item) { int i = _list.IndexOf(item); if (i < 0) return false; RemoveAt(i); return true; }
        public void RemoveAt(int i) { if (!Allowed()) return; var old = _list[i]; _list.RemoveAt(i); Record(new SyncListChange<T>(SyncListOperation.Removed, default, old, i)); }
        public void Clear() { if (!Allowed()) return; _list.Clear(); Record(new SyncListChange<T>(SyncListOperation.Cleared, default, default, 0)); }
        public bool Contains(T item) => _list.Contains(item);
        public int IndexOf(T item) => _list.IndexOf(item);
        public void CopyTo(T[] a, int i) => _list.CopyTo(a, i);
        public IEnumerator<T> GetEnumerator() => _list.GetEnumerator();
        IEnumerator IEnumerable.GetEnumerator() => _list.GetEnumerator();

        bool Allowed()
        {
            if (identity == null || !identity.isSpawned || isController) return true;
            Debug.LogWarning($"{behaviour.GetType().Name}.SyncList<{typeof(T).Name}>: only the {(ownerAuth ? "owner" : "server")} may write it");
            return false;
        }

        void Record(SyncListChange<T> c)
        {
            if (c.operation == SyncListOperation.Cleared) _pending.Clear();
            if (_pending.Count >= 255) { _pending.Clear(); _pending.Add(new SyncListChange<T>(SyncListOperation.Cleared, default, default, -1)); }
            _pending.Add(c); isDirty = true; onChanged?.Invoke(c);
        }

        public override void WriteFull(BitWriter w)
        {
            w.WriteUInt((ulong)_list.Count, 16);
            foreach (var x in _list) NetSerializer.Write(w, x, typeof(T));
        }

        public override void ReadFull(BitReader r)
        {
            _list.Clear(); _pending.Clear();
            int n = (int)r.ReadUInt(16);
            for (int i = 0; i < n; i++) _list.Add((T)NetSerializer.Read(r, typeof(T)));
            onChanged?.Invoke(new SyncListChange<T>(SyncListOperation.Cleared, default, default, -1));
        }

        public override void WriteDelta(BitWriter w)
        {
            // index -1 Cleared = "resend everything" marker after an overflow
            if (_pending.Count > 0 && _pending[0].operation == SyncListOperation.Cleared && _pending[0].index == -1)
            { w.WriteBits(0, 8); WriteFull(w); _pending.Clear(); return; }
            w.WriteBits((ulong)_pending.Count, 8);
            foreach (var c in _pending)
            {
                w.WriteBits((byte)c.operation, 3); w.WriteUInt((ulong)Math.Max(0, c.index), 16);
                if (c.operation == SyncListOperation.Added || c.operation == SyncListOperation.Inserted || c.operation == SyncListOperation.Set) NetSerializer.Write(w, c.value, typeof(T));
            }
            _pending.Clear();
        }

        public override void ReadDelta(BitReader r)
        {
            int n = (int)r.ReadBits(8);
            if (n == 0) { ReadFull(r); return; }
            for (int k = 0; k < n; k++)
            {
                var op = (SyncListOperation)r.ReadBits(3); int i = (int)r.ReadUInt(16);
                switch (op)
                {
                    case SyncListOperation.Added: { var v = (T)NetSerializer.Read(r, typeof(T)); _list.Add(v); onChanged?.Invoke(new SyncListChange<T>(op, v, default, _list.Count - 1)); break; }
                    case SyncListOperation.Inserted: { var v = (T)NetSerializer.Read(r, typeof(T)); i = Math.Min(i, _list.Count); _list.Insert(i, v); onChanged?.Invoke(new SyncListChange<T>(op, v, default, i)); break; }
                    case SyncListOperation.Set: { var v = (T)NetSerializer.Read(r, typeof(T)); if (i < _list.Count) { var old = _list[i]; _list[i] = v; onChanged?.Invoke(new SyncListChange<T>(op, v, old, i)); } break; }
                    case SyncListOperation.Removed: { if (i < _list.Count) { var old = _list[i]; _list.RemoveAt(i); onChanged?.Invoke(new SyncListChange<T>(op, default, old, i)); } break; }
                    case SyncListOperation.Cleared: _list.Clear(); onChanged?.Invoke(new SyncListChange<T>(op, default, default, 0)); break;
                }
            }
        }
    }

    public enum SyncDictionaryOperation : byte { Set, Removed, Cleared }

    public readonly struct SyncDictionaryChange<K, V>
    {
        public readonly SyncDictionaryOperation operation; public readonly K key; public readonly V value;
        public SyncDictionaryChange(SyncDictionaryOperation op, K key, V value) { operation = op; this.key = key; this.value = value; }
    }

    public sealed class SyncDictionary<K, V> : NetworkModule, IEnumerable<KeyValuePair<K, V>>
    {
        readonly Dictionary<K, V> _dict = new Dictionary<K, V>();
        readonly List<SyncDictionaryChange<K, V>> _pending = new List<SyncDictionaryChange<K, V>>();
        public event Action<SyncDictionaryChange<K, V>> onChanged;

        public SyncDictionary(bool ownerAuth = false, bool ownerOnly = false) { this.ownerAuth = ownerAuth; this.ownerOnly = ownerOnly; }

        public int Count => _dict.Count;
        public IEnumerable<K> Keys => _dict.Keys;
        public IEnumerable<V> Values => _dict.Values;
        public bool ContainsKey(K k) => _dict.ContainsKey(k);
        public bool TryGetValue(K k, out V v) => _dict.TryGetValue(k, out v);
        public V this[K k] { get => _dict[k]; set => Set(k, value); }
        public void Add(K k, V v) => Set(k, v);

        public void Set(K k, V v)
        {
            if (!Allowed()) return;
            _dict[k] = v; Record(new SyncDictionaryChange<K, V>(SyncDictionaryOperation.Set, k, v));
        }

        public bool Remove(K k)
        {
            if (!Allowed() || !_dict.Remove(k)) return false;
            Record(new SyncDictionaryChange<K, V>(SyncDictionaryOperation.Removed, k, default)); return true;
        }

        public void Clear() { if (!Allowed()) return; _dict.Clear(); _pending.Clear(); Record(new SyncDictionaryChange<K, V>(SyncDictionaryOperation.Cleared, default, default)); }
        public IEnumerator<KeyValuePair<K, V>> GetEnumerator() => _dict.GetEnumerator();
        IEnumerator IEnumerable.GetEnumerator() => _dict.GetEnumerator();

        bool Allowed()
        {
            if (identity == null || !identity.isSpawned || isController) return true;
            Debug.LogWarning($"{behaviour.GetType().Name}.SyncDictionary: only the {(ownerAuth ? "owner" : "server")} may write it");
            return false;
        }

        void Record(SyncDictionaryChange<K, V> c) { if (_pending.Count >= 255) { _pending.Clear(); _pending.Add(new SyncDictionaryChange<K, V>(SyncDictionaryOperation.Cleared, default, default)); foreach (var kv in _dict) _pending.Add(new SyncDictionaryChange<K, V>(SyncDictionaryOperation.Set, kv.Key, kv.Value)); } else _pending.Add(c); isDirty = true; onChanged?.Invoke(c); }

        public override void WriteFull(BitWriter w)
        {
            w.WriteUInt((ulong)_dict.Count, 16);
            foreach (var kv in _dict) { NetSerializer.Write(w, kv.Key, typeof(K)); NetSerializer.Write(w, kv.Value, typeof(V)); }
        }

        public override void ReadFull(BitReader r)
        {
            _dict.Clear(); _pending.Clear();
            int n = (int)r.ReadUInt(16);
            for (int i = 0; i < n; i++) { var k = (K)NetSerializer.Read(r, typeof(K)); _dict[k] = (V)NetSerializer.Read(r, typeof(V)); }
            onChanged?.Invoke(new SyncDictionaryChange<K, V>(SyncDictionaryOperation.Cleared, default, default));
        }

        public override void WriteDelta(BitWriter w)
        {
            w.WriteBits((ulong)_pending.Count, 9);
            foreach (var c in _pending)
            {
                w.WriteBits((byte)c.operation, 2);
                if (c.operation != SyncDictionaryOperation.Cleared) NetSerializer.Write(w, c.key, typeof(K));
                if (c.operation == SyncDictionaryOperation.Set) NetSerializer.Write(w, c.value, typeof(V));
            }
            _pending.Clear();
        }

        public override void ReadDelta(BitReader r)
        {
            int n = (int)r.ReadBits(9);
            for (int i = 0; i < n; i++)
            {
                var op = (SyncDictionaryOperation)r.ReadBits(2);
                switch (op)
                {
                    case SyncDictionaryOperation.Set: { var k = (K)NetSerializer.Read(r, typeof(K)); var v = (V)NetSerializer.Read(r, typeof(V)); _dict[k] = v; onChanged?.Invoke(new SyncDictionaryChange<K, V>(op, k, v)); break; }
                    case SyncDictionaryOperation.Removed: { var k = (K)NetSerializer.Read(r, typeof(K)); _dict.Remove(k); onChanged?.Invoke(new SyncDictionaryChange<K, V>(op, k, default)); break; }
                    case SyncDictionaryOperation.Cleared: _dict.Clear(); onChanged?.Invoke(new SyncDictionaryChange<K, V>(op, default, default)); break;
                }
            }
        }
    }
}
