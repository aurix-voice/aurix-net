// Aurix Net Unity SDK — per-player level-of-detail for replication traffic.
// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    [Serializable]
    public struct NetworkLODTier
    {
        [Tooltip("Players whose closest owned object is within this distance get this tier (or a closer one).")] public float maxDistance;
        [Tooltip("Extra distance a player must move before dropping back to a closer tier (avoids flapping).")] public float hysteresis;
        [Tooltip("Send dirty deltas to players in this tier every N server ticks.")] public int sendIntervalTicks;
        public NetworkLODTier(float maxDistance, int sendIntervalTicks, float hysteresis = 0f) { this.maxDistance = maxDistance; this.sendIntervalTicks = Math.Max(1, sendIntervalTicks); this.hysteresis = hysteresis; }
    }

    /// <summary>
    /// Server-side send-rate tiers by distance: far players get this object's deltas less often (or not at all when
    /// culled beyond the last tier). Does not change visibility: culled players stay observers and still get spawn,
    /// despawn, ownership and RPCs; only module deltas are gated.
    /// </summary>
    public class NetworkLOD : NetworkBehaviour
    {
        public const byte CulledTier = byte.MaxValue;

        public NetworkLODTier[] tiers = { new NetworkLODTier(25f, 1), new NetworkLODTier(75f, 3, 5f), new NetworkLODTier(200f, 10, 10f) };
        [Tooltip("Players farther than the last tier get no deltas at all.")] public bool cullBeyondLastTier = true;
        [Tooltip("Re-evaluate tiers every N server ticks.")] public int evaluateIntervalTicks = 10;

        readonly Dictionary<PlayerID, byte> _tiers = new Dictionary<PlayerID, byte>();
        public event Action<PlayerID, byte, byte> onTierChanged;

        /// <summary>Tier of a player (0 = closest / full rate; owner is always 0).</summary>
        public byte GetTier(PlayerID p) { if (identity != null && identity.owner == p) return 0; return _tiers.TryGetValue(p, out var t) ? t : (byte)0; }
        public bool IsCulled(PlayerID p) => GetTier(p) == CulledTier;

        /// <summary>Whether a dirty delta of this object should go to the player on this server tick.</summary>
        public bool ShouldSendToPlayer(PlayerID p, uint tick)
        {
            byte t = GetTier(p);
            if (t == CulledTier) return false;
            if (tiers == null || t >= tiers.Length) return true;
            int every = Math.Max(1, tiers[t].sendIntervalTicks);
            if (every == 1) return true;
            uint stagger = identity != null ? identity.objectId : 0;
            return (tick + stagger) % (uint)every == 0;
        }

        protected override void OnBeforeBind() { if (identity != null) identity.lod = this; }
        protected override void OnSpawned(bool asServer) { if (identity != null) identity.lod = this; }
        protected override void OnDespawned() { _tiers.Clear(); }

        public override void OnTick(uint tick)
        {
            if (!isServer || networkManager == null || tiers == null || tiers.Length == 0) return;
            if (evaluateIntervalTicks > 1 && tick % (uint)evaluateIntervalTicks != 0) return;
            var me = transform.position;
            foreach (var p in identity.observers)
            {
                if (identity.owner == p) continue;
                float best = float.MaxValue;
                foreach (var id in networkManager.identities) if (id.owner == p) { float d = Vector3.Distance(id.transform.position, me); if (d < best) best = d; }
                if (best == float.MaxValue) continue;  // no avatar: keep the current tier
                byte prev = _tiers.TryGetValue(p, out var pt) ? pt : (byte)0, next = Resolve(best, prev);
                if (next == prev) { _tiers[p] = next; continue; }
                _tiers[p] = next; onTierChanged?.Invoke(p, prev, next);
            }
        }

        byte Resolve(float dist, byte current)
        {
            for (int i = 0; i < tiers.Length; i++)
            {
                float limit = tiers[i].maxDistance;
                if (current != CulledTier && i < current) limit += tiers[i].hysteresis;  // moving closer needs margin
                if (dist <= limit) return (byte)i;
            }
            return cullBeyondLastTier ? CulledTier : (byte)(tiers.Length - 1);
        }

        internal void RemovePlayer(PlayerID p) => _tiers.Remove(p);
    }
}
