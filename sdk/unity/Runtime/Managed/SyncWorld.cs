// Client-side replicated world for sync rooms: decodes server deltas (0x11), keeps entities,
// acks (0x13), and encodes owner inputs (0x10 with full component writes). Engine-agnostic.
using System;
using System.Collections.Generic;

namespace Aurix.Net.Managed
{
    public sealed class NetEntity
    {
        public ulong Id;
        public ushort? Owner;
        public uint LastTick;
        public Dictionary<ushort, ComponentValue> Components =
            new Dictionary<ushort, ComponentValue>();

        public ComponentValue Get(ComponentDef component) =>
            component != null && Components.TryGetValue(component.Id, out var value) ? value : null;
    }

    /// <summary>Per-entity ownership policy: what the cell does when the owner leaves and who may take the entity.</summary>
    public enum OwnerPolicy : byte
    {
        DestroyOnLeave = 0,
        ToMaster = 1,
        Orphan = 2,
        Dynamic = 3,
    }

    /// <summary>One owner-authoritative op for <see cref="SyncWorld.EncodeOps"/>.</summary>
    public struct SyncOp
    {
        public const byte Write = 0,
            Spawn = 1,
            Despawn = 2,
            Transfer = 3,
            SetPolicy = 4,
            Request = 5,
            Release = 6,
            SpawnTagged = 7;

        /// <summary>Ownership policy: 0 destroy on leave (default), 1 to master, 2 orphan, 3 dynamic.</summary>
        public byte Policy;
        public byte Op;
        public uint Tag;
        public ulong Entity;
        public ushort To;
        public IList<(ComponentDef, ComponentValue)> Writes;

        public static SyncOp WriteEntity(
            ulong entity,
            IList<(ComponentDef, ComponentValue)> writes
        ) =>
            new SyncOp
            {
                Op = Write,
                Entity = entity,
                Writes = writes,
            };

        public static SyncOp SpawnEntity(IList<(ComponentDef, ComponentValue)> writes = null) =>
            new SyncOp { Op = Spawn, Writes = writes };

        public static SyncOp SpawnTaggedEntity(
            uint tag,
            IList<(ComponentDef, ComponentValue)> writes = null
        ) =>
            new SyncOp
            {
                Op = SpawnTagged,
                Tag = tag,
                Writes = writes,
            };

        public static SyncOp DespawnEntity(ulong entity) =>
            new SyncOp { Op = Despawn, Entity = entity };

        /// <summary>Hands an entity you own to another actor in the room.</summary>
        public static SyncOp TransferEntity(ulong entity, ushort to) =>
            new SyncOp
            {
                Op = Transfer,
                Entity = entity,
                To = to,
            };

        public static SyncOp SetEntityPolicy(ulong entity, OwnerPolicy policy) =>
            new SyncOp
            {
                Op = SetPolicy,
                Entity = entity,
                Policy = (byte)policy,
            };

        public static SyncOp RequestOwnership(ulong entity) =>
            new SyncOp { Op = Request, Entity = entity };

        public static SyncOp ReleaseEntity(ulong entity) =>
            new SyncOp { Op = Release, Entity = entity };
    }

    /// <summary>Refused: the actor's entity limit, or the room is full. Invalid: the room could not read the
    /// spawn.</summary>
    public enum SpawnStatus : byte
    {
        Created = 0,
        Refused = 1,
        Invalid = 2,
    }

    /// <summary>The room's answer to a tagged spawn: the entity it made for the tag, or why it made none. Tag 0
    /// names the avatar.</summary>
    public struct SpawnAnswer
    {
        public uint Tag;
        public SpawnStatus Status;

        /// <summary>The entity as deltas name it. Read it only when created: 0 is an entity too.</summary>
        public ulong Entity;

        /// <summary>The tick the room ran the spawn at.</summary>
        public uint Tick;
    }

    /// <summary>The tagged spawns of one stay in a room: hands out tags and keeps each spawn until the room
    /// answers it.</summary>
    public sealed class PendingSpawns
    {
        readonly List<(uint Tag, byte[] Body, uint Tick)> _unanswered =
            new List<(uint, byte[], uint)>();
        uint _next = 1;

        /// <summary>`firstFree` is the first tag the room has not seen from this actor (`joined.next_spawn_tag`).</summary>
        public uint NextTag(uint firstFree)
        {
            uint tag = Math.Max(_next, firstFree);
            _next = tag + 1;
            return tag;
        }

        /// <summary>`tick` is the one the input with the spawn was stamped with.</summary>
        public void Sent(uint tag, byte[] body, uint tick) => _unanswered.Add((tag, body, tick));

        /// <summary>False for an answer to a spawn already answered: the room answers every repeat.</summary>
        public bool Take(SpawnAnswer answer)
        {
            return answer.Tag == 0 || _unanswered.RemoveAll(spawn => spawn.Tag == answer.Tag) > 0;
        }

        /// <summary>Sends again every spawn the room passed `after` ticks ago with no answer: the room makes
        /// nothing new out of a tag it knows. `send` returns the tick the repeat is stamped with.</summary>
        public void Repeat(uint serverTick, uint after, Func<byte[], uint> send)
        {
            for (int i = 0; i < _unanswered.Count; i++)
            {
                var spawn = _unanswered[i];
                if (serverTick > spawn.Tick + after)
                {
                    _unanswered[i] = (spawn.Tag, spawn.Body, send(spawn.Body));
                }
            }
        }
    }

    public sealed class SyncWorld
    {
        public const byte KindInput = 0x10,
            KindDelta = 0x11,
            KindResync = 0x12,
            KindAck = 0x13,
            KindSpawnAnswer = 0x14;
        public readonly Schema Schema;
        public readonly Dictionary<ulong, NetEntity> Entities = new Dictionary<ulong, NetEntity>();
        public uint ServerTick { get; private set; }
        public long Gaps { get; private set; }
        uint _lastDelta;
        uint _history;
        bool _any;

        /// <summary>Entity first seen / changed in this delta / removed.</summary>
        public event Action<NetEntity> OnSpawn,
            OnChange,
            OnDespawn;

        public SyncWorld(Schema schema)
        {
            Schema = schema;
        }

        /// <summary>Feeds any sync datagram payload; returns the ack to send back (null when not a delta).</summary>
        public byte[] Receive(byte[] payload)
        {
            if (payload.Length < 11 || payload[0] != KindDelta)
            {
                return null;
            }

            var reader = new BitReader(payload, 1);
            uint tick = (uint)reader.ReadBits(32),
                baseline = (uint)reader.ReadBits(32);
            ServerTick = tick;
            if (!_any)
            {
                _any = true;
                _lastDelta = tick;
                _history = 0;
            }
            else if (tick > _lastDelta)
            {
                uint sh = tick - _lastDelta;
                if (baseline != 0 && baseline - 1 != _lastDelta && !Acked(baseline - 1))
                {
                    Gaps++;
                }

                _history = sh >= 32 ? 0 : (_history << (int)sh) | (1u << (int)(sh - 1));
                _lastDelta = tick;
            }
            else if (tick < _lastDelta)
            {
                uint behind = _lastDelta - tick;
                if (behind <= 32)
                {
                    _history |= 1u << (int)(behind - 1);
                }
            }
            int nd = (int)reader.ReadBits(16);
            for (int i = 0; i < nd; i++)
            {
                ulong id = reader.ReadBits(48);
                if (Entities.TryGetValue(id, out var entity))
                {
                    Entities.Remove(id);
                    OnDespawn?.Invoke(entity);
                }
            }
            int count = (int)reader.ReadBits(16);
            int cidBits = Schema.ComponentIdBits;
            for (int i = 0; i < count; i++)
            {
                ulong id = reader.ReadBits(48);
                bool fresh = !Entities.TryGetValue(id, out var entity);
                if (fresh)
                {
                    entity = new NetEntity { Id = id };
                    Entities[id] = entity;
                }
                entity.Owner = reader.ReadBool() ? (ushort?)reader.ReadBits(16) : null;
                entity.LastTick = tick;
                int nc = (int)reader.ReadBits(8);
                for (int k = 0; k < nc; k++)
                {
                    ushort cid = (ushort)reader.ReadBits(cidBits);
                    var component =
                        Schema.ById(cid)
                        ?? throw new NetException("delta: unknown component " + cid);
                    if (reader.ReadBool())
                    {
                        entity.Components.Remove(cid);
                        continue;
                    }
                    bool full = reader.ReadBool();
                    if (full)
                    {
                        entity.Components[cid] = ComponentValue.DecodeFull(component, reader);
                    }
                    else
                    {
                        entity.Components[cid] = ComponentValue.DecodeDelta(
                            entity.Components.TryGetValue(cid, out var @base)
                                ? @base
                                : ComponentValue.Zero(component),
                            component,
                            reader
                        );
                    }
                }
                if (fresh)
                {
                    OnSpawn?.Invoke(entity);
                }
                else
                {
                    OnChange?.Invoke(entity);
                }
            }
            return EncodeAck(_lastDelta, _history);
        }

        bool Acked(uint tick)
        {
            if (tick == _lastDelta)
            {
                return true;
            }

            if (tick > _lastDelta)
            {
                return false;
            }

            uint behind = _lastDelta - tick;
            return behind <= 32 && (_history & (1u << (int)(behind - 1))) != 0;
        }

        /// <summary>The room's answer to a tagged spawn, or null when the payload is not one.</summary>
        public static SpawnAnswer? ReadSpawnAnswer(byte[] payload)
        {
            if (payload.Length != 18 || payload[0] != KindSpawnAnswer)
            {
                return null;
            }

            ulong Read(int at, int bytes)
            {
                ulong value = 0;
                for (int i = 0; i < bytes; i++)
                {
                    value = (value << 8) | payload[at + i];
                }
                return value;
            }

            return new SpawnAnswer
            {
                Tag = (uint)Read(1, 4),
                Status = payload[5] <= 1 ? (SpawnStatus)payload[5] : SpawnStatus.Invalid,
                Entity = Read(6, 8),
                Tick = (uint)Read(14, 4),
            };
        }

        public static byte[] EncodeAck(uint last, uint bits) =>
            new byte[]
            {
                KindAck,
                (byte)(last >> 24),
                (byte)(last >> 16),
                (byte)(last >> 8),
                (byte)last,
                (byte)(bits >> 24),
                (byte)(bits >> 16),
                (byte)(bits >> 8),
                (byte)bits,
            };

        /// <summary>Input packet: the last inputs ending at `firstTick + inputs.Count - 1` (send 2-3 for redundancy).</summary>
        public static byte[] EncodeInput(uint firstTick, IList<byte[]> inputs)
        {
            int len = 6;
            foreach (var input in inputs)
            {
                len += 1 + Math.Min(255, input.Length);
            }

            var bytes = new byte[len];
            int offset = 0;
            bytes[offset++] = KindInput;
            bytes[offset++] = (byte)(firstTick >> 24);
            bytes[offset++] = (byte)(firstTick >> 16);
            bytes[offset++] = (byte)(firstTick >> 8);
            bytes[offset++] = (byte)firstTick;
            bytes[offset++] = (byte)Math.Min(255, inputs.Count);
            foreach (var input in inputs)
            {
                int count = Math.Min(255, input.Length);
                bytes[offset++] = (byte)count;
                Array.Copy(input, 0, bytes, offset, count);
                offset += count;
            }
            return bytes;
        }

        /// <summary>Owner-authoritative input body: full writes of the given components (applied to the actor's avatar).</summary>
        public byte[] EncodeWrites(IList<(ComponentDef, ComponentValue)> writes)
        {
            var writer = new BitWriter();
            writer.WriteBits((ulong)writes.Count, 8);
            int cb = Schema.ComponentIdBits;
            foreach (var (component, value) in writes)
            {
                writer.WriteBits(component.Id, cb);
                value.EncodeFull(component, writer);
            }
            return writer.Finish();
        }

        /// <summary>Owner-authoritative ops: write/spawn/despawn/transfer entities you own (at most 32 spawned per actor).</summary>
        public byte[] EncodeOps(IList<SyncOp> ops)
        {
            var writer = new BitWriter();
            writer.WriteBits(0xff, 8);
            writer.WriteBits((ulong)ops.Count, 8);
            int cb = Schema.ComponentIdBits;
            foreach (var op in ops)
            {
                writer.WriteBits(op.Op, 8);
                if (op.Op == SyncOp.SpawnTagged)
                {
                    writer.WriteBits(op.Tag, 32);
                }
                else if (op.Op != SyncOp.Spawn)
                {
                    writer.WriteBits(op.Entity, 48);
                }

                if (op.Op == SyncOp.Transfer)
                {
                    writer.WriteBits(op.To, 16);
                    continue;
                }
                if (op.Op == SyncOp.SetPolicy)
                {
                    writer.WriteBits(op.Policy, 8);
                    continue;
                }
                if (op.Op == SyncOp.Request || op.Op == SyncOp.Release)
                {
                    continue;
                }

                if (op.Op == SyncOp.Despawn)
                {
                    continue;
                }

                var ws = op.Writes ?? Array.Empty<(ComponentDef, ComponentValue)>();
                writer.WriteBits((ulong)ws.Count, 8);
                foreach (var (component, value) in ws)
                {
                    writer.WriteBits(component.Id, cb);
                    value.EncodeFull(component, writer);
                }
            }
            return writer.Finish();
        }
    }
}
