// A networked object: one NetworkIdentity on the prefab root, any number of NetworkBehaviours
// below it. Spawned and despawned by the server (room host); ownership gives one player control
// of ownerAuth modules and the right to call ServerRpcs that require it.
using System;
using System.Collections.Generic;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    [DisallowMultipleComponent]
    public class NetworkIdentity : MonoBehaviour
    {
        [Tooltip("Behaviours replicated with this object, in the same order on every peer. Filled from children when empty.")]
        public List<NetworkBehaviour> behaviours = new List<NetworkBehaviour>();
        [Tooltip("Despawn this object when its owner leaves (otherwise ownership is just removed).")]
        public bool despawnOnOwnerDisconnect = true;

        public NetworkManager networkManager { get; private set; }
        public uint objectId { get; private set; }
        public int prefabId { get; private set; } = -1;
        public PlayerID? owner { get; private set; }
        public bool isSpawned { get; private set; }

        public bool isServer => isSpawned && networkManager.isServer;
        public bool isClient => isSpawned && networkManager.isClient;
        public bool isHost => isSpawned && networkManager.isHost;
        public bool hasOwner => owner.HasValue;
        public bool isOwner => isSpawned && networkManager.localPlayer.HasValue && owner == networkManager.localPlayer;
        /// <summary>The peer that drives this object: its owner when it has one, else the server.</summary>
        public bool isController => isSpawned && (hasOwner ? isOwner : isServer);
        public bool IsController(bool ownerHasAuthority) => ownerHasAuthority ? isController : isServer;
        public PlayerID? localPlayer => networkManager?.localPlayer;

        public event Action<PlayerID?, PlayerID?> onOwnerChanged;
        public event Action onSpawned, onDespawned;

        protected virtual void Awake() { if (behaviours.Count == 0) GetComponentsInChildren(true, behaviours); }

        internal void Setup(NetworkManager m, uint id, int prefab, PlayerID? own)
        {
            networkManager = m; objectId = id; prefabId = prefab; owner = own;
            if (behaviours.Count == 0) GetComponentsInChildren(true, behaviours);
            for (int i = 0; i < behaviours.Count; i++) behaviours[i].Bind(this, i);
        }

        internal void MarkSpawned(bool asServer)
        {
            isSpawned = true;
            foreach (var b in behaviours) b.InternalSpawned(asServer);
            onSpawned?.Invoke();
        }

        internal void MarkDespawned()
        {
            if (!isSpawned) return;
            isSpawned = false;
            foreach (var b in behaviours) b.InternalDespawned();
            onDespawned?.Invoke();
        }

        internal void SetOwner(PlayerID? o)
        {
            if (owner == o) return;
            var old = owner; owner = o;
            foreach (var b in behaviours) b.InternalOwnerChanged(old, o);
            onOwnerChanged?.Invoke(old, o);
        }

        /// <summary>Server only: hand the object to a player (null = server-controlled).</summary>
        public void GiveOwnership(PlayerID? player) => networkManager?.SetOwnership(this, player);
        public void RemoveOwnership() => GiveOwnership(null);
        /// <summary>Server only: remove the object everywhere.</summary>
        public void Despawn() => networkManager?.Despawn(this);

        public override string ToString() => $"{name}#{objectId}{(hasOwner ? " owner " + owner : "")}";
    }
}
