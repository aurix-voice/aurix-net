// Interest management: on the server every spawned object has a set of observers (players that
// hold a copy). Rules decide who sees what; the manager re-evaluates them every
// visibilityIntervalTicks and spawns/despawns on the clients accordingly. Owners always see their
// own objects; the host's local player sees everything.
using System.Collections.Generic;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    public interface INetworkVisibilityRule
    {
        bool CanSee(PlayerID player, NetworkIdentity target);
    }

    public sealed class AlwaysVisibleRule : INetworkVisibilityRule { public bool CanSee(PlayerID p, NetworkIdentity t) => true; }
    public sealed class NoVisibilityRule : INetworkVisibilityRule { public bool CanSee(PlayerID p, NetworkIdentity t) => false; }
    /// <summary>Only the owner (and the server) see the object.</summary>
    public sealed class OwnerOnlyRule : INetworkVisibilityRule { public bool CanSee(PlayerID p, NetworkIdentity t) => t.owner == p; }

    /// <summary>Visible when within <see cref="distance"/> of any object the player owns (its avatar).</summary>
    public sealed class DistanceRule : INetworkVisibilityRule
    {
        public float distance = 50f;
        readonly NetworkManager _m;
        public DistanceRule(NetworkManager m, float distance = 50f) { _m = m; this.distance = distance; }

        public bool CanSee(PlayerID p, NetworkIdentity t)
        {
            var tp = Position(t); float best = float.MaxValue;
            foreach (var id in _m.identities) if (id.owner == p) { float d = Vector3.Distance(Position(id), tp); if (d < best) best = d; }
            return best <= distance;
        }

        static Vector3 Position(NetworkIdentity id)
        {
            foreach (var b in id.behaviours) if (b is NetworkTransform nt && nt.sync != null) return nt.sync.current.pos;
            return id.transform.position;
        }
    }
}
