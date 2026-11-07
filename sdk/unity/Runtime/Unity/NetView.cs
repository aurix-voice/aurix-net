// Identity of a replicated entity on a GameObject (like a network object id + owner).
using System.Collections.Generic;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity
{
    [DisallowMultipleComponent]
    public sealed class NetView : MonoBehaviour
    {
        public ulong EntityId { get; internal set; }
        public ushort? Owner { get; internal set; }
        public NetEntity Entity { get; internal set; }
        public NetRunner Runner { get; internal set; }
        /// <summary>True when the local player owns this entity (its inputs drive it).</summary>
        public bool IsMine => Runner != null && Owner.HasValue && Owner.Value == Runner.Actor;
        internal readonly List<SyncBehaviour> Behaviours = new List<SyncBehaviour>();

        internal void Bind(NetRunner runner, NetEntity e)
        {
            Runner = runner; Entity = e; EntityId = e.Id; Owner = e.Owner;
            Behaviours.Clear(); GetComponentsInChildren(true, Behaviours);
            foreach (var b in Behaviours) b.Bind(this);
        }

        /// <summary>Invokes a [Rpc] method on every peer's copy of this view (reliable relay event).</summary>
        public void Rpc(string method, params object[] args) => Runner.SendRpc(this, method, args);
    }
}
