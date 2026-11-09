// Aurix Net Unity SDK — ValidatedSyncVar (owner proposes, server validates) and SyncBigData (chunked large payloads).
// SPDX-License-Identifier: Apache-2.0
using System;
using UnityEngine;
using Aurix.Net.Managed;

namespace Aurix.Net.Unity.Framework
{
    /// <summary>
    /// Owner-authoritative sync var whose writes are checked on the server: <see cref="validate"/>(current, proposed)
    /// returns false to reject, in which case the server sends its value back to the owner (which reverts) and
    /// observers never see the proposal. Without a validator it behaves like an owner-auth <see cref="SyncVar{T}"/>.
    /// </summary>
    public sealed class ValidatedSyncVar<T> : NetworkModule
    {
        T _value;
        public event Action<T> onChanged;
        /// <summary>Owner side: the server rejected a value (argument: the value it was reverted to).</summary>
        public event Action<T> onRejected;
        /// <summary>Server side: (current, proposed) → accept?</summary>
        public Func<T, T, bool> validate;
        /// <summary>Server side: fired when a proposal was rejected (owner, proposed).</summary>
        public event Action<PlayerID?, T> onServerRejected;
        bool _awaitingServer;

        public ValidatedSyncVar(T initialValue = default, float sendIntervalInSeconds = 0f, Func<T, T, bool> validate = null)
        {
            _value = initialValue; this.sendIntervalInSeconds = sendIntervalInSeconds; this.validate = validate; ownerAuth = true;
        }

        public T value
        {
            get => _value;
            set
            {
                if (NetSerializer.AreEqual(_value, value)) return;
                if (identity != null && identity.isSpawned && !isController) { Debug.LogWarning($"ValidatedSyncVar<{typeof(T).Name}>: only the owner may write it"); return; }
                _value = value; isDirty = true; _awaitingServer = identity != null && identity.isSpawned && !identity.isServer;
                onChanged?.Invoke(value);
            }
        }

        public override void WriteFull(BitWriter w) => NetSerializer.Write(w, _value, typeof(T));
        public override void ReadFull(BitReader r) => Apply((T)NetSerializer.Read(r, typeof(T)), false);
        public override void WriteDelta(BitWriter w) => WriteFull(w);
        public override void ReadDelta(BitReader r)
        {
            var v = (T)NetSerializer.Read(r, typeof(T));
            bool correction = isController && _awaitingServer;   // owner receiving the server's answer
            _awaitingServer = false;
            Apply(v, correction);
        }

        public override bool ReadDeltaFromOwner(BitReader r)
        {
            var proposed = (T)NetSerializer.Read(r, typeof(T));
            if (validate != null && !validate(_value, proposed)) { onServerRejected?.Invoke(identity?.owner, proposed); return false; }
            Apply(proposed, false); return true;
        }

        void Apply(T v, bool correction)
        {
            if (NetSerializer.AreEqual(_value, v)) return;
            _value = v; onChanged?.Invoke(v);
            if (correction) onRejected?.Invoke(v);
        }

        public static implicit operator T(ValidatedSyncVar<T> s) => s._value;
        public override string ToString() => _value?.ToString() ?? "null";
    }

    /// <summary>
    /// Replicates a large byte payload (texture, save file, level blob) in chunks over the reliable sync path.
    /// Controller calls <see cref="Set"/>; receivers get <see cref="progress"/> updates and <see cref="onCompleted"/>.
    /// Late joiners receive the whole payload with the spawn state.
    /// </summary>
    public sealed class SyncBigData : NetworkModule
    {
        public int chunkSize = 8 * 1024;
        [Tooltip("Chunks sent per tick.")] public int chunksPerTick = 2;
        byte[] _data = Array.Empty<byte>(); uint _version;
        byte[] _incoming; uint _incomingVersion; int _received, _nextChunk, _totalChunks;
        public event Action<byte[]> onCompleted;
        public event Action<float> onProgress;

        public SyncBigData(bool ownerAuth = false, int chunkSize = 8 * 1024) { this.ownerAuth = ownerAuth; this.chunkSize = Math.Max(64, chunkSize); }

        /// <summary>Latest complete payload on this peer (empty until the first transfer finishes).</summary>
        public byte[] data => _data;
        public uint version => _version;
        public bool isTransferring => _incoming != null;
        public float progress => _incoming == null ? 1f : _totalChunks == 0 ? 1f : (float)_received / _totalChunks;

        /// <summary>Controller: replace the payload; it streams to every observer in chunks.</summary>
        public void Set(byte[] bytes)
        {
            if (identity != null && identity.isSpawned && !isController) { Debug.LogWarning("SyncBigData: only the controller may set it"); return; }
            _data = bytes ?? Array.Empty<byte>(); _version++;
            _nextChunk = 0; _totalChunks = (_data.Length + chunkSize - 1) / chunkSize; isDirty = true;
        }

        public override void OnBeforeSend() { if (_nextChunk < _totalChunks) isDirty = true; }

        public override void WriteFull(BitWriter w) { w.WriteBits(_version, 32); w.WriteUInt((ulong)_data.Length, 32); w.WriteBytes(_data); }
        public override void ReadFull(BitReader r)
        {
            _version = (uint)r.ReadBits(32); int n = (int)r.ReadUInt(32); _data = n > 0 ? r.ReadBytes(n) : Array.Empty<byte>();
            _incoming = null; onProgress?.Invoke(1f); onCompleted?.Invoke(_data);
        }

        public override void WriteDelta(BitWriter w)
        {
            int count = Math.Max(0, Math.Min(chunksPerTick, _totalChunks - _nextChunk));
            w.WriteBits(_version, 32); w.WriteUInt((ulong)_data.Length, 32); w.WriteUInt((ulong)_totalChunks, 16); w.WriteUInt((ulong)_nextChunk, 16); w.WriteUInt((ulong)count, 8);
            for (int c = 0; c < count; c++, _nextChunk++)
            {
                int off = _nextChunk * chunkSize, len = Math.Min(chunkSize, _data.Length - off);
                w.WriteUInt((ulong)len, 16); for (int i = 0; i < len; i++) w.WriteBits(_data[off + i], 8);
            }
        }

        public override void ReadDelta(BitReader r)
        {
            uint ver = (uint)r.ReadBits(32); int total = (int)r.ReadUInt(32), chunks = (int)r.ReadUInt(16), first = (int)r.ReadUInt(16), count = (int)r.ReadUInt(8);
            if (_incoming == null || _incomingVersion != ver) { _incoming = new byte[total]; _incomingVersion = ver; _received = 0; _totalChunks = chunks; }
            for (int c = 0; c < count; c++)
            {
                int len = (int)r.ReadUInt(16), off = (first + c) * chunkSize;
                for (int i = 0; i < len; i++) { byte b = (byte)r.ReadBits(8); if (off + i < _incoming.Length) _incoming[off + i] = b; }
                _received++;
            }
            onProgress?.Invoke(progress);
            if (_received >= _totalChunks || total == 0) { _data = _incoming; _version = ver; _incoming = null; onCompleted?.Invoke(_data); }
        }
    }
}
