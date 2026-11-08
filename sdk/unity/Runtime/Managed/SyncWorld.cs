// Client-side replicated world for sync rooms: decodes server deltas (0x11), keeps entities,
// acks (0x13), and encodes owner inputs (0x10 with full component writes). Engine-agnostic.
using System;
using System.Collections.Generic;

namespace Aurix.Net.Managed
{
    public sealed class NetEntity
    {
        public ulong Id; public ushort? Owner; public uint LastTick;
        public Dictionary<ushort, ComponentValue> Components = new Dictionary<ushort, ComponentValue>();
        public ComponentValue Get(ComponentDef c) => c != null && Components.TryGetValue(c.Id, out var v) ? v : null;
    }

    /// <summary>One owner-authoritative op for <see cref="SyncWorld.EncodeOps"/>.</summary>
    public struct SyncOp
    {
        public const byte Write = 0, Spawn = 1, Despawn = 2, Transfer = 3;
        public byte Op; public ulong Entity; public ushort To; public IList<(ComponentDef, ComponentValue)> Writes;
        public static SyncOp WriteEntity(ulong entity, IList<(ComponentDef, ComponentValue)> writes) => new SyncOp { Op = Write, Entity = entity, Writes = writes };
        public static SyncOp SpawnEntity(IList<(ComponentDef, ComponentValue)> writes = null) => new SyncOp { Op = Spawn, Writes = writes };
        public static SyncOp DespawnEntity(ulong entity) => new SyncOp { Op = Despawn, Entity = entity };
        /// <summary>Hands an entity you own to another actor in the room.</summary>
        public static SyncOp TransferEntity(ulong entity, ushort to) => new SyncOp { Op = Transfer, Entity = entity, To = to };
    }

    public sealed class SyncWorld
    {
        public const byte KindInput = 0x10, KindDelta = 0x11, KindResync = 0x12, KindAck = 0x13;
        public readonly Schema Schema;
        public readonly Dictionary<ulong, NetEntity> Entities = new Dictionary<ulong, NetEntity>();
        public uint ServerTick { get; private set; }
        public long Gaps { get; private set; }
        uint _lastDelta; uint _history; bool _any;
        /// <summary>Entity first seen / changed in this delta / removed.</summary>
        public event Action<NetEntity> OnSpawn, OnChange, OnDespawn;

        public SyncWorld(Schema schema) { Schema = schema; }

        /// <summary>Feeds any sync datagram payload; returns the ack to send back (null when not a delta).</summary>
        public byte[] Receive(byte[] p)
        {
            if (p.Length < 11 || p[0] != KindDelta) return null;
            var r = new BitReader(p, 1);
            uint tick = (uint)r.ReadBits(32), baseline = (uint)r.ReadBits(32);
            ServerTick = tick;
            if (!_any) { _any = true; _lastDelta = tick; _history = 0; }
            else if (tick > _lastDelta)
            {
                uint sh = tick - _lastDelta;
                if (baseline != 0 && baseline - 1 != _lastDelta && !Acked(baseline - 1)) Gaps++;
                _history = sh >= 32 ? 0 : (_history << (int)sh) | (1u << (int)(sh - 1)); _lastDelta = tick;
            }
            else if (tick < _lastDelta) { uint d = _lastDelta - tick; if (d <= 32) _history |= 1u << (int)(d - 1); }
            int nd = (int)r.ReadBits(16);
            for (int i = 0; i < nd; i++) { ulong id = r.ReadBits(48); if (Entities.TryGetValue(id, out var e)) { Entities.Remove(id); OnDespawn?.Invoke(e); } }
            int n = (int)r.ReadBits(16); int cidBits = Schema.ComponentIdBits;
            for (int i = 0; i < n; i++)
            {
                ulong id = r.ReadBits(48); bool fresh = !Entities.TryGetValue(id, out var e);
                if (fresh) { e = new NetEntity { Id = id }; Entities[id] = e; }
                e.Owner = r.ReadBool() ? (ushort?)r.ReadBits(16) : null; e.LastTick = tick;
                int nc = (int)r.ReadBits(8);
                for (int k = 0; k < nc; k++)
                {
                    ushort cid = (ushort)r.ReadBits(cidBits); var c = Schema.ById(cid) ?? throw new NetException("delta: unknown component " + cid);
                    if (r.ReadBool()) { e.Components.Remove(cid); continue; }
                    bool full = r.ReadBool();
                    if (full) e.Components[cid] = ComponentValue.DecodeFull(c, r);
                    else e.Components[cid] = ComponentValue.DecodeDelta(e.Components.TryGetValue(cid, out var b) ? b : ComponentValue.Zero(c), c, r);
                }
                if (fresh) OnSpawn?.Invoke(e); else OnChange?.Invoke(e);
            }
            return EncodeAck(_lastDelta, _history);
        }
        bool Acked(uint t) { if (t == _lastDelta) return true; if (t > _lastDelta) return false; uint d = _lastDelta - t; return d <= 32 && (_history & (1u << (int)(d - 1))) != 0; }

        public static byte[] EncodeAck(uint last, uint bits) => new byte[] { KindAck, (byte)(last >> 24), (byte)(last >> 16), (byte)(last >> 8), (byte)last, (byte)(bits >> 24), (byte)(bits >> 16), (byte)(bits >> 8), (byte)bits };

        /// <summary>Input packet: the last inputs ending at `firstTick + inputs.Count - 1` (send 2-3 for redundancy).</summary>
        public static byte[] EncodeInput(uint firstTick, IList<byte[]> inputs)
        {
            int len = 6; foreach (var i in inputs) len += 1 + Math.Min(255, i.Length);
            var b = new byte[len]; int o = 0; b[o++] = KindInput; b[o++] = (byte)(firstTick >> 24); b[o++] = (byte)(firstTick >> 16); b[o++] = (byte)(firstTick >> 8); b[o++] = (byte)firstTick; b[o++] = (byte)Math.Min(255, inputs.Count);
            foreach (var i in inputs) { int n = Math.Min(255, i.Length); b[o++] = (byte)n; Array.Copy(i, 0, b, o, n); o += n; }
            return b;
        }

        /// <summary>Owner-authoritative input body: full writes of the given components (applied to the actor's avatar).</summary>
        public byte[] EncodeWrites(IList<(ComponentDef, ComponentValue)> writes)
        {
            var w = new BitWriter(); w.WriteBits((ulong)writes.Count, 8); int cb = Schema.ComponentIdBits;
            foreach (var (c, v) in writes) { w.WriteBits(c.Id, cb); v.EncodeFull(c, w); }
            return w.Finish();
        }

        /// <summary>Owner-authoritative ops: write/spawn/despawn/transfer entities you own (at most 32 spawned per actor).</summary>
        public byte[] EncodeOps(IList<SyncOp> ops)
        {
            var w = new BitWriter(); w.WriteBits(0xff, 8); w.WriteBits((ulong)ops.Count, 8); int cb = Schema.ComponentIdBits;
            foreach (var op in ops)
            {
                w.WriteBits(op.Op, 8);
                if (op.Op != SyncOp.Spawn) w.WriteBits(op.Entity, 48);
                if (op.Op == SyncOp.Transfer) { w.WriteBits(op.To, 16); continue; }
                if (op.Op == SyncOp.Despawn) continue;
                var ws = op.Writes ?? Array.Empty<(ComponentDef, ComponentValue)>();
                w.WriteBits((ulong)ws.Count, 8);
                foreach (var (c, v) in ws) { w.WriteBits(c.Id, cb); v.EncodeFull(c, w); }
            }
            return w.Finish();
        }
    }
}
