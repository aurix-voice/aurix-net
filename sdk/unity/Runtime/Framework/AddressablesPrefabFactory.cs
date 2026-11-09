// Aurix Net Unity SDK — network prefabs loaded through Addressables (compiled only when com.unity.addressables is present).
// SPDX-License-Identifier: Apache-2.0
#if AURIX_ADDRESSABLES
using System;
using System.Collections.Generic;
using UnityEngine;
using UnityEngine.AddressableAssets;
using UnityEngine.ResourceManagement.AsyncOperations;

namespace Aurix.Net.Unity.Framework
{
    /// <summary>
    /// Prefab ids map to <see cref="AssetReferenceGameObject"/>s; spawns for not-yet-loaded prefabs stay pending on the
    /// client until the asset arrives. Assign to <c>NetworkManager.factory</c> and register the same list on every peer.
    /// </summary>
    [Serializable]
    public sealed class AddressablesPrefabFactory : IAsyncNetworkObjectFactory
    {
        public List<AssetReferenceGameObject> prefabs = new List<AssetReferenceGameObject>();
        readonly Dictionary<int, GameObject> _loaded = new Dictionary<int, GameObject>();
        readonly Dictionary<int, AsyncOperationHandle<GameObject>> _handles = new Dictionary<int, AsyncOperationHandle<GameObject>>();
        readonly Dictionary<int, List<Action<GameObject>>> _waiting = new Dictionary<int, List<Action<GameObject>>>();

        /// <summary>Preload a prefab (e.g. during a loading screen) so later spawns are synchronous.</summary>
        public void Preload(int prefabId) => Load(prefabId, null);

        public NetworkIdentity Instantiate(NetworkManager m, int prefabId)
        {
            if (!_loaded.TryGetValue(prefabId, out var prefab)) throw new NetException($"prefab {prefabId} is not loaded yet; use InstantiateAsync");
            return Make(prefab, prefabId);
        }

        public void InstantiateAsync(NetworkManager m, int prefabId, Action<NetworkIdentity> ready)
        {
            if (_loaded.TryGetValue(prefabId, out var prefab)) { ready(Make(prefab, prefabId)); return; }
            Load(prefabId, go => ready(go != null ? Make(go, prefabId) : null));
        }

        public void Destroy(NetworkIdentity id) { if (id != null) UnityEngine.Object.Destroy(id.gameObject); }

        /// <summary>Release every loaded prefab (call when leaving the game).</summary>
        public void ReleaseAll() { foreach (var h in _handles.Values) if (h.IsValid()) Addressables.Release(h); _handles.Clear(); _loaded.Clear(); }

        void Load(int prefabId, Action<GameObject> done)
        {
            if (prefabId < 0 || prefabId >= prefabs.Count || prefabs[prefabId] == null) { Debug.LogError($"AddressablesPrefabFactory: unknown prefab id {prefabId}"); done?.Invoke(null); return; }
            if (_waiting.TryGetValue(prefabId, out var list)) { if (done != null) list.Add(done); return; }
            list = new List<Action<GameObject>>(); if (done != null) list.Add(done); _waiting[prefabId] = list;
            var h = prefabs[prefabId].LoadAssetAsync<GameObject>(); _handles[prefabId] = h;
            h.Completed += op =>
            {
                _waiting.Remove(prefabId);
                if (op.Status == AsyncOperationStatus.Succeeded) _loaded[prefabId] = op.Result; else Debug.LogError($"AddressablesPrefabFactory: failed to load prefab {prefabId}: {op.OperationException?.Message}");
                foreach (var cb in list) cb(op.Status == AsyncOperationStatus.Succeeded ? op.Result : null);
            };
        }

        static NetworkIdentity Make(GameObject prefab, int prefabId)
        {
            var go = UnityEngine.Object.Instantiate(prefab);
            var id = go.GetComponent<NetworkIdentity>();
            if (id == null) throw new NetException(prefab.name + " has no NetworkIdentity");
            return id;
        }
    }
}
#endif
