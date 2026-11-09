// Inspector for NetworkManager: settings when editing, live role/players/objects and Start/Stop buttons in play mode.
// Not yet exercised in a Unity Editor (no licence on the build machine); kept to plain IMGUI APIs.
using System.Linq;
using Aurix.Net.Unity.Framework;
using UnityEditor;
using UnityEngine;

namespace Aurix.Net.Unity.Editor
{
    [CustomEditor(typeof(NetworkManager))]
    public class NetworkManagerEditor : UnityEditor.Editor
    {
        bool _objects = true;

        public override void OnInspectorGUI()
        {
            DrawDefaultInspector();
            var m = (NetworkManager)target;
            if (!Application.isPlaying) { EditorGUILayout.HelpBox("Enter play mode to start a host, server or client from here.", MessageType.None); return; }

            EditorGUILayout.Space();
            EditorGUILayout.LabelField("Runtime", EditorStyles.boldLabel);
            string role = m.isHost ? "Host" : m.isServer ? "Server" : m.isClient ? "Client" : "Offline";
            EditorGUILayout.LabelField("Role", role);
            EditorGUILayout.LabelField("Local player", m.localPlayer?.ToString() ?? "-");
            EditorGUILayout.LabelField("Server player", m.serverPlayer?.ToString() ?? "-");
            EditorGUILayout.LabelField("Tick / server tick", $"{m.tick} / {m.serverTick}");
            EditorGUILayout.LabelField("Players", string.Join(", ", m.players.Select(p => p.ToString())));

            using (new EditorGUILayout.HorizontalScope())
            {
                using (new EditorGUI.DisabledScope(m.isServer || m.isClient))
                {
                    if (GUILayout.Button("Start Host")) m.StartHost();
                    if (GUILayout.Button("Start Server")) m.StartServer();
                    if (GUILayout.Button("Start Client")) m.StartClient();
                }
                using (new EditorGUI.DisabledScope(!(m.isServer || m.isClient)))
                    if (GUILayout.Button("Stop")) m.Stop();
            }

            var ids = m.identities.ToList();
            _objects = EditorGUILayout.Foldout(_objects, $"Objects ({ids.Count})", true);
            if (_objects)
                foreach (var id in ids.OrderBy(i => i.objectId))
                {
                    using (new EditorGUILayout.HorizontalScope())
                    {
                        EditorGUILayout.ObjectField(id, typeof(NetworkIdentity), true, GUILayout.MinWidth(120));
                        EditorGUILayout.LabelField($"#{id.objectId}  owner {(id.owner?.ToString() ?? "server")}  observers {id.observers.Count}", GUILayout.MinWidth(200));
                    }
                }
            Repaint();
        }
    }
}
