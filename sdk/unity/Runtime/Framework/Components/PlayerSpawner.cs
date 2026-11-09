// Server-side helper: spawns one `playerPrefab` per player when they join (owner = that player) at
// the next spawn point, and, through NetworkIdentity.despawnOnOwnerDisconnect, lets it go when
// they leave. Put it next to the NetworkManager.
using System.Collections.Generic;
using Aurix.Net.Transports;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    public class PlayerSpawner : MonoBehaviour
    {
        public NetworkManager manager;
        [Tooltip("Prefab (from NetworkManager.networkPrefabs) or its index when prefabId >= 0.")]
        public GameObject playerPrefab;
        public int prefabId = -1;
        [Tooltip("Spawn points used round-robin; empty = origin.")]
        public List<Transform> spawnPoints = new List<Transform>();
        [Tooltip("Also spawn an object for the host's own player.")]
        public bool spawnForHost = true;

        readonly Dictionary<PlayerID, NetworkIdentity> _spawned = new Dictionary<PlayerID, NetworkIdentity>();
        public IReadOnlyDictionary<PlayerID, NetworkIdentity> spawned => _spawned;
        int _next;

        void Awake() { if (manager == null) manager = GetComponent<NetworkManager>() ?? NetworkManager.main; }
        void OnEnable() => Attach(manager);
        void OnDisable() => Detach();

        public void Attach(NetworkManager m)
        {
            Detach(); manager = m; if (m == null) return;
            m.onServerState += OnServerState; m.onPlayerJoined += OnJoined; m.onPlayerLeft += OnLeft;
        }

        public void Detach()
        {
            if (manager == null) return;
            manager.onServerState -= OnServerState; manager.onPlayerJoined -= OnJoined; manager.onPlayerLeft -= OnLeft;
        }

        void OnServerState(BridgeState s)
        {
            if (s == BridgeState.Started && manager.isHost && spawnForHost && manager.localPlayer.HasValue) OnJoined(manager.localPlayer.Value);
            if (s == BridgeState.Stopped) _spawned.Clear();
        }

        void OnJoined(PlayerID p)
        {
            if (!manager.isServer || _spawned.ContainsKey(p)) return;
            int pid = prefabId >= 0 ? prefabId : manager.networkPrefabs.IndexOf(playerPrefab);
            if (pid < 0) { Debug.LogWarning("[AurixNet] PlayerSpawner: no player prefab"); return; }
            var id = manager.Spawn(pid, p);
            if (spawnPoints.Count > 0) { var sp = spawnPoints[_next++ % spawnPoints.Count]; id.transform.position = sp.position; id.transform.rotation = sp.rotation; foreach (var b in id.behaviours) if (b is NetworkTransform nt) nt.Teleport(sp.position, sp.rotation); }
            _spawned[p] = id;
        }

        void OnLeft(PlayerID p) => _spawned.Remove(p);
    }
}
