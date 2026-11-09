// Inspector for NetworkIdentity: scene objects get a stable sceneId (hash of scene + hierarchy path),
// play mode shows object id, owner and observers. Not yet exercised in a Unity Editor.
using System.Linq;
using Aurix.Net.Unity.Framework;
using UnityEditor;
using UnityEditor.SceneManagement;
using UnityEngine;

namespace Aurix.Net.Unity.Editor
{
    [CustomEditor(typeof(NetworkIdentity), true)]
    public class NetworkIdentityEditor : UnityEditor.Editor
    {
        public override void OnInspectorGUI()
        {
            var id = (NetworkIdentity)target;
            if (!Application.isPlaying && SceneIds.IsSceneObject(id))
            {
                uint want = SceneIds.Compute(id);
                if (id.sceneId != want)
                {
                    Undo.RecordObject(id, "Assign scene id"); id.sceneId = want; EditorUtility.SetDirty(id);
                    if (!Application.isPlaying) EditorSceneManager.MarkSceneDirty(id.gameObject.scene);
                }
                EditorGUILayout.LabelField("Scene id", id.sceneId.ToString());
            }
            DrawDefaultInspector();
            if (!Application.isPlaying) return;
            EditorGUILayout.Space();
            EditorGUILayout.LabelField("Runtime", EditorStyles.boldLabel);
            EditorGUILayout.LabelField("Object id", id.isSpawned ? id.objectId.ToString() : "not spawned");
            EditorGUILayout.LabelField("Owner", id.owner?.ToString() ?? "server");
            EditorGUILayout.LabelField("Is owner / server / client", $"{id.isOwner} / {id.isServer} / {id.isClient}");
            if (id.isServer) EditorGUILayout.LabelField("Observers", string.Join(", ", id.observers.Select(p => p.ToString())));
            Repaint();
        }
    }

    /// <summary>Stable per-scene ids for objects that live in the scene (not prefabs): FNV-1a of "scenePath|hierarchyPath", never 0.</summary>
    public static class SceneIds
    {
        public static bool IsSceneObject(NetworkIdentity id) =>
            id != null && id.gameObject.scene.IsValid() && !string.IsNullOrEmpty(id.gameObject.scene.path) && !PrefabUtility.IsPartOfPrefabAsset(id.gameObject);

        public static uint Compute(NetworkIdentity id)
        {
            var path = id.gameObject.scene.path + "|" + HierarchyPath(id.transform);
            uint h = 2166136261;
            foreach (char c in path) { h ^= c; h *= 16777619; }
            return h == 0 ? 1u : h;
        }

        static string HierarchyPath(Transform t)
        {
            var s = t.name;
            while (t.parent != null) { t = t.parent; s = t.name + "/" + s; }
            return s;
        }

        [MenuItem("Aurix Net/Assign scene ids in open scenes")]
        public static void AssignAll()
        {
            int n = 0;
            foreach (var id in Object.FindObjectsByType<NetworkIdentity>(FindObjectsInactive.Include, FindObjectsSortMode.None))
                if (IsSceneObject(id)) { uint want = Compute(id); if (id.sceneId != want) { Undo.RecordObject(id, "Assign scene id"); id.sceneId = want; EditorUtility.SetDirty(id); n++; } }
            Debug.Log($"[AurixNet] scene ids assigned: {n}");
        }
    }
}
