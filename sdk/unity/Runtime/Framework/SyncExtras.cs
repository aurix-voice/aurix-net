// ValidatedSyncVar (owner proposes, server validates) and SyncBigData (chunked large payloads).
// SPDX-License-Identifier: Apache-2.0
using System;
using Aurix.Net.Managed;
using UnityEngine;

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

        public ValidatedSyncVar(
            T initialValue = default,
            float sendIntervalInSeconds = 0f,
            Func<T, T, bool> validate = null
        )
        {
            _value = initialValue;
            this.sendIntervalInSeconds = sendIntervalInSeconds;
            this.validate = validate;
            ownerAuth = true;
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
                        $"ValidatedSyncVar<{typeof(T).Name}>: only the owner may write it"
                    );
                    return;
                }
                _value = value;
                isDirty = true;
                _awaitingServer = identity != null && identity.isSpawned && !identity.isServer;
                onChanged?.Invoke(value);
            }
        }

        public override void WriteFull(BitWriter writer) =>
            NetSerializer.Write(writer, _value, typeof(T));

        public override void ReadFull(BitReader reader) =>
            Apply((T)NetSerializer.Read(reader, typeof(T)), false);

        public override void WriteDelta(BitWriter writer) => WriteFull(writer);

        public override void ReadDelta(BitReader reader)
        {
            var value = (T)NetSerializer.Read(reader, typeof(T));
            bool correction = isController && _awaitingServer; // owner receiving the server's answer
            _awaitingServer = false;
            Apply(value, correction);
        }

        public override bool ReadDeltaFromOwner(BitReader reader)
        {
            var proposed = (T)NetSerializer.Read(reader, typeof(T));
            if (validate != null && !validate(_value, proposed))
            {
                onServerRejected?.Invoke(identity?.owner, proposed);
                return false;
            }
            Apply(proposed, false);
            return true;
        }

        void Apply(T value, bool correction)
        {
            if (NetSerializer.AreEqual(_value, value))
            {
                return;
            }

            _value = value;
            onChanged?.Invoke(value);
            if (correction)
            {
                onRejected?.Invoke(value);
            }
        }

        public static implicit operator T(ValidatedSyncVar<T> source) => source._value;

        public override string ToString() => _value?.ToString() ?? "null";
    }

    /// <summary>
    /// Owner → server input value (not replicated to other observers). The owner writes <see cref="value"/> whenever the
    /// input changes; the server gets <see cref="onChanged"/>. Use it for non-predicted input (menus, abilities, chat
    /// typing state); predicted movement uses <c>PredictedBehaviour</c>.
    /// </summary>
    public sealed class SyncInput<T> : NetworkModule
    {
        T _value;
        public event Action<T> onChanged;

        /// <summary>Server tick at which the latest input was received.</summary>
        public uint lastInputTick { get; private set; }

        public SyncInput(T defaultValue = default, float sendIntervalInSeconds = 0f)
        {
            _value = defaultValue;
            sendIntervalInSeconds = Math.Max(0, sendIntervalInSeconds);
            this.sendIntervalInSeconds = sendIntervalInSeconds;
            ownerAuth = true;
            serverOnly = true;
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

                if (identity != null && identity.isSpawned && !identity.isOwner)
                {
                    Debug.LogWarning($"SyncInput<{typeof(T).Name}>: only the owner may write it");
                    return;
                }
                _value = value;
                isDirty = true;
                onChanged?.Invoke(value);
            }
        }

        public override void WriteFull(BitWriter writer) =>
            NetSerializer.Write(writer, _value, typeof(T));

        public override void ReadFull(BitReader reader)
        {
            _value = (T)NetSerializer.Read(reader, typeof(T));
        }

        public override void WriteDelta(BitWriter writer) => WriteFull(writer);

        public override void ReadDelta(BitReader reader)
        {
            var value = (T)NetSerializer.Read(reader, typeof(T));
            lastInputTick = lastReceivedTick;
            if (NetSerializer.AreEqual(_value, value))
            {
                return;
            }

            _value = value;
            onChanged?.Invoke(value);
        }

        public static implicit operator T(SyncInput<T> source) => source._value;
    }

    /// <summary>Replicates a Texture2D (PNG-encoded) through <see cref="SyncBigData"/>; remotes get a decoded texture in <see cref="onChanged"/>.</summary>
    public sealed class SyncTexture : NetworkModule
    {
        readonly SyncBigData _data;
        public Texture2D texture { get; private set; }
        public event Action<Texture2D> onChanged;
        public float progress => _data.progress;
        public bool isTransferring => _data.isTransferring;

        public SyncTexture(bool ownerAuth = false, int chunkSize = 8 * 1024)
        {
            this.ownerAuth = ownerAuth;
            _data = new SyncBigData(ownerAuth, chunkSize);
            _data.onCompleted += bytes =>
            {
                if (bytes.Length == 0)
                {
                    texture = null;
                    onChanged?.Invoke(null);
                    return;
                }
                var decoded = new Texture2D(2, 2);
                if (decoded.LoadImage(bytes))
                {
                    texture = decoded;
                    onChanged?.Invoke(decoded);
                }
            };
        }

        /// <summary>Controller: replace the texture (encoded to PNG and streamed).</summary>
        public void Set(Texture2D tex)
        {
            if (identity != null && identity.isSpawned && !isController)
            {
                Debug.LogWarning("SyncTexture: only the controller may set it");
                return;
            }
            texture = tex;
            _data.Set(tex != null ? tex.EncodeToPNG() : Array.Empty<byte>());
            isDirty = true;
        }

        public override void OnBeforeSend()
        {
            _data.OnBeforeSend();
            if (_data.isDirty)
            {
                isDirty = true;
            }
        }

        public override void WriteFull(BitWriter writer) => _data.WriteFull(writer);

        public override void ReadFull(BitReader reader) => _data.ReadFull(reader);

        public override void WriteDelta(BitWriter writer)
        {
            _data.WriteDelta(writer);
            _data.ClearDirty();
        }

        public override void ReadDelta(BitReader reader) => _data.ReadDelta(reader);
    }

    /// <summary>Replicates a named file (name + bytes) through <see cref="SyncBigData"/>.</summary>
    public sealed class SyncFile : NetworkModule
    {
        readonly SyncBigData _data;
        public string fileName { get; private set; } = "";
        public byte[] bytes => _data.data;
        public event Action<string, byte[]> onChanged;
        public float progress => _data.progress;
        public bool isTransferring => _data.isTransferring;
        string _incomingName;

        public SyncFile(bool ownerAuth = false, int chunkSize = 8 * 1024)
        {
            this.ownerAuth = ownerAuth;
            _data = new SyncBigData(ownerAuth, chunkSize);
            _data.onCompleted += bytes =>
            {
                fileName = _incomingName ?? fileName;
                onChanged?.Invoke(fileName, bytes);
            };
        }

        public void Set(string name, byte[] content)
        {
            if (identity != null && identity.isSpawned && !isController)
            {
                Debug.LogWarning("SyncFile: only the controller may set it");
                return;
            }
            fileName = name ?? "";
            _data.Set(content);
            isDirty = true;
        }

        public override void OnBeforeSend()
        {
            _data.OnBeforeSend();
            if (_data.isDirty)
            {
                isDirty = true;
            }
        }

        public override void WriteFull(BitWriter writer)
        {
            NetSerializer.WriteString(writer, fileName);
            _data.WriteFull(writer);
        }

        public override void ReadFull(BitReader reader)
        {
            _incomingName = NetSerializer.ReadString(reader);
            _data.ReadFull(reader);
        }

        public override void WriteDelta(BitWriter writer)
        {
            NetSerializer.WriteString(writer, fileName);
            _data.WriteDelta(writer);
            _data.ClearDirty();
        }

        public override void ReadDelta(BitReader reader)
        {
            _incomingName = NetSerializer.ReadString(reader);
            _data.ReadDelta(reader);
        }
    }

    /// <summary>
    /// Replicates a large byte payload (texture, save file, level blob) in chunks over the reliable sync path.
    /// Controller calls <see cref="Set"/>; receivers get <see cref="progress"/> updates and <see cref="onCompleted"/>.
    /// Late joiners receive the whole payload with the spawn state.
    /// </summary>
    public sealed class SyncBigData : NetworkModule
    {
        public int chunkSize = 8 * 1024;

        [Tooltip("Chunks sent per tick.")]
        public int chunksPerTick = 2;
        byte[] _data = Array.Empty<byte>();
        uint _version;
        byte[] _incoming;
        uint _incomingVersion;
        int _received,
            _nextChunk,
            _totalChunks;
        public event Action<byte[]> onCompleted;
        public event Action<float> onProgress;

        public SyncBigData(bool ownerAuth = false, int chunkSize = 8 * 1024)
        {
            this.ownerAuth = ownerAuth;
            this.chunkSize = Math.Max(64, chunkSize);
        }

        /// <summary>Latest complete payload on this peer (empty until the first transfer finishes).</summary>
        public byte[] data => _data;
        public uint version => _version;
        public bool isTransferring => _incoming != null;
        public float progress =>
            _incoming == null ? 1f
            : _totalChunks == 0 ? 1f
            : (float)_received / _totalChunks;

        /// <summary>Controller: replace the payload; it streams to every observer in chunks.</summary>
        public void Set(byte[] bytes)
        {
            if (identity != null && identity.isSpawned && !isController)
            {
                Debug.LogWarning("SyncBigData: only the controller may set it");
                return;
            }
            _data = bytes ?? Array.Empty<byte>();
            _version++;
            _nextChunk = 0;
            _totalChunks = (_data.Length + chunkSize - 1) / chunkSize;
            isDirty = true;
        }

        public override void OnBeforeSend()
        {
            if (_nextChunk < _totalChunks)
            {
                isDirty = true;
            }
        }

        public override void WriteFull(BitWriter writer)
        {
            writer.WriteBits(_version, 32);
            writer.WriteUInt((ulong)_data.Length, 32);
            writer.WriteBytes(_data);
        }

        public override void ReadFull(BitReader reader)
        {
            _version = (uint)reader.ReadBits(32);
            int count = (int)reader.ReadUInt(32);
            _data = count > 0 ? reader.ReadBytes(count) : Array.Empty<byte>();
            _incoming = null;
            onProgress?.Invoke(1f);
            onCompleted?.Invoke(_data);
        }

        public override void WriteDelta(BitWriter writer)
        {
            int count = Math.Max(0, Math.Min(chunksPerTick, _totalChunks - _nextChunk));
            writer.WriteBits(_version, 32);
            writer.WriteUInt((ulong)_data.Length, 32);
            writer.WriteUInt((ulong)_totalChunks, 16);
            writer.WriteUInt((ulong)_nextChunk, 16);
            writer.WriteUInt((ulong)count, 8);
            for (int chunk = 0; chunk < count; chunk++, _nextChunk++)
            {
                int off = _nextChunk * chunkSize,
                    len = Math.Min(chunkSize, _data.Length - off);
                writer.WriteUInt((ulong)len, 16);
                for (int i = 0; i < len; i++)
                {
                    writer.WriteBits(_data[off + i], 8);
                }
            }
        }

        public override void ReadDelta(BitReader reader)
        {
            uint ver = (uint)reader.ReadBits(32);
            int total = (int)reader.ReadUInt(32),
                chunks = (int)reader.ReadUInt(16),
                first = (int)reader.ReadUInt(16),
                count = (int)reader.ReadUInt(8);
            if (_incoming == null || _incomingVersion != ver)
            {
                _incoming = new byte[total];
                _incomingVersion = ver;
                _received = 0;
                _totalChunks = chunks;
            }
            for (int chunk = 0; chunk < count; chunk++)
            {
                int len = (int)reader.ReadUInt(16),
                    off = (first + chunk) * chunkSize;
                for (int i = 0; i < len; i++)
                {
                    byte value = (byte)reader.ReadBits(8);
                    if (off + i < _incoming.Length)
                    {
                        _incoming[off + i] = value;
                    }
                }
                _received++;
            }
            onProgress?.Invoke(progress);
            if (_received >= _totalChunks || total == 0)
            {
                _data = _incoming;
                _version = ver;
                _incoming = null;
                onCompleted?.Invoke(_data);
            }
        }
    }
}
