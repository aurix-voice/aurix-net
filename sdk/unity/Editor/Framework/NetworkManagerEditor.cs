// Inspector for NetworkManager: settings when editing, live role/players/objects and Start/Stop buttons in play mode.
// Not yet run in a Unity editor; kept to plain IMGUI APIs.
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
            var manager = (NetworkManager)target;
            if (!Application.isPlaying)
            {
                EditorGUILayout.HelpBox(
                    "Enter play mode to start a host, server or client from here.",
                    MessageType.None
                );
                return;
            }

            EditorGUILayout.Space();
            EditorGUILayout.LabelField("Runtime", EditorStyles.boldLabel);
            string role =
                manager.isHost ? "Host"
                : manager.isServer ? "Server"
                : manager.isClient ? "Client"
                : "Offline";
            EditorGUILayout.LabelField("Role", role);
            EditorGUILayout.LabelField("Local player", manager.localPlayer?.ToString() ?? "-");
            EditorGUILayout.LabelField("Server player", manager.serverPlayer?.ToString() ?? "-");
            EditorGUILayout.LabelField(
                "Tick / server tick",
                $"{manager.tick} / {manager.serverTick}"
            );
            EditorGUILayout.LabelField(
                "Players",
                string.Join(", ", manager.players.Select(player => player.ToString()))
            );

            using (new EditorGUILayout.HorizontalScope())
            {
                using (new EditorGUI.DisabledScope(manager.isServer || manager.isClient))
                {
                    if (GUILayout.Button("Start Host"))
                    {
                        manager.StartHost();
                    }

                    if (GUILayout.Button("Start Server"))
                    {
                        manager.StartServer();
                    }

                    if (GUILayout.Button("Start Client"))
                    {
                        manager.StartClient();
                    }
                }
                using (new EditorGUI.DisabledScope(!(manager.isServer || manager.isClient)))
                {
                    if (GUILayout.Button("Stop"))
                    {
                        manager.Stop();
                    }
                }
            }

            var ids = manager.identities.ToList();
            _objects = EditorGUILayout.Foldout(_objects, $"Objects ({ids.Count})", true);
            if (_objects)
            {
                foreach (var id in ids.OrderBy(identity => identity.objectId))
                {
                    using (new EditorGUILayout.HorizontalScope())
                    {
                        EditorGUILayout.ObjectField(
                            id,
                            typeof(NetworkIdentity),
                            true,
                            GUILayout.MinWidth(120)
                        );
                        EditorGUILayout.LabelField(
                            $"#{id.objectId}  owner {(id.owner?.ToString() ?? "server")}  observers {id.observers.Count}",
                            GUILayout.MinWidth(200)
                        );
                    }
                }
            }

            Repaint();
        }
    }
}
