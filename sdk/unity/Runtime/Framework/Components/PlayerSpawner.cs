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

        readonly Dictionary<PlayerID, NetworkIdentity> _spawned =
            new Dictionary<PlayerID, NetworkIdentity>();
        public IReadOnlyDictionary<PlayerID, NetworkIdentity> spawned => _spawned;
        int _next;

        void Awake()
        {
            if (manager == null)
            {
                manager = GetComponent<NetworkManager>() ?? NetworkManager.main;
            }
        }

        void OnEnable() => Attach(manager);

        void OnDisable() => Detach();

        public void Attach(NetworkManager networkManager)
        {
            Detach();
            manager = networkManager;
            if (networkManager == null)
            {
                return;
            }

            networkManager.onServerState += OnServerState;
            networkManager.onPlayerJoined += OnJoined;
            networkManager.onPlayerLeft += OnLeft;
        }

        public void Detach()
        {
            if (manager == null)
            {
                return;
            }

            manager.onServerState -= OnServerState;
            manager.onPlayerJoined -= OnJoined;
            manager.onPlayerLeft -= OnLeft;
        }

        void OnServerState(BridgeState state)
        {
            if (
                state == BridgeState.Started
                && manager.isHost
                && spawnForHost
                && manager.localPlayer.HasValue
            )
            {
                OnJoined(manager.localPlayer.Value);
            }

            if (state == BridgeState.Stopped)
            {
                _spawned.Clear();
            }
        }

        void OnJoined(PlayerID player)
        {
            if (!manager.isServer || _spawned.ContainsKey(player))
            {
                return;
            }

            int pid = prefabId >= 0 ? prefabId : manager.networkPrefabs.IndexOf(playerPrefab);
            if (pid < 0)
            {
                Debug.LogWarning("[AurixNet] PlayerSpawner: no player prefab");
                return;
            }
            var id = manager.Spawn(pid, player);
            if (spawnPoints.Count > 0)
            {
                var sp = spawnPoints[_next++ % spawnPoints.Count];
                id.transform.position = sp.position;
                id.transform.rotation = sp.rotation;
                foreach (var behaviour in id.behaviours)
                {
                    if (behaviour is NetworkTransform nt)
                    {
                        nt.Teleport(sp.position, sp.rotation);
                    }
                }
            }
            _spawned[player] = id;
        }

        void OnLeft(PlayerID player) => _spawned.Remove(player);
    }
}
