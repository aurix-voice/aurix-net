// Editor window showing live NetworkStatistics of the running NetworkManager.
// SPDX-License-Identifier: Apache-2.0
using Aurix.Net.Unity.Framework;
using UnityEditor;
using UnityEngine;

namespace Aurix.Net.Unity.Editor
{
    public sealed class NetworkStatisticsWindow : EditorWindow
    {
        [MenuItem("Window/Aurix Net/Network Statistics")]
        static void Open() => GetWindow<NetworkStatisticsWindow>("Aurix Net Stats");

        void OnInspectorUpdate() => Repaint();

        void OnGUI()
        {
            var manager = NetworkManager.main;
            if (manager == null || (!manager.isServer && !manager.isClient))
            {
                EditorGUILayout.HelpBox(
                    "No running NetworkManager (enter play mode and start host/server/client).",
                    MessageType.Info
                );
                return;
            }
            var statistics = manager.statistics;
            EditorGUILayout.LabelField(
                "Role",
                manager.isHost ? "Host"
                    : manager.isServer ? "Server"
                    : "Client"
            );
            EditorGUILayout.LabelField("Server tick", manager.serverTick.ToString());
            EditorGUILayout.LabelField("Players", manager.players.Count.ToString());
            EditorGUILayout.LabelField(
                "Objects",
                manager.identities
                    is System.Collections.Generic.ICollection<NetworkIdentity> collection
                    ? collection.Count.ToString()
                    : "-"
            );
            EditorGUILayout.Space();
            if (manager.isClient && !manager.isServer)
            {
                EditorGUILayout.LabelField(
                    "Ping",
                    statistics.hasPingEstimate
                        ? $"{statistics.ping} ms (last {statistics.lastPing}, jitter {statistics.jitter})"
                        : "measuring…"
                );
                if (statistics.isHighPing)
                {
                    EditorGUILayout.HelpBox("High ping", MessageType.Warning);
                }

                if (statistics.isHighJitter)
                {
                    EditorGUILayout.HelpBox("High jitter", MessageType.Warning);
                }
            }
            if (statistics.isConnectionStalled)
            {
                EditorGUILayout.HelpBox(
                    $"Connection stalled: nothing received for {statistics.secondsSinceLastReceived:0.0}s",
                    MessageType.Error
                );
            }

            EditorGUILayout.LabelField(
                "Upload",
                $"{statistics.upload / 1024f:0.0} KB/s, {statistics.messagesOutPerSecond} msg/s"
            );
            EditorGUILayout.LabelField(
                "Download",
                $"{statistics.download / 1024f:0.0} KB/s, {statistics.messagesInPerSecond} msg/s"
            );
            EditorGUILayout.LabelField(
                "Total",
                $"{statistics.totalBytesOut / 1024f:0.0} KB out, {statistics.totalBytesIn / 1024f:0.0} KB in"
            );
            if (manager.isServer && statistics.playerPings.Count > 0)
            {
                EditorGUILayout.Space();
                EditorGUILayout.LabelField("Player pings", EditorStyles.boldLabel);
                foreach (var kv in statistics.playerPings)
                {
                    EditorGUILayout.LabelField(kv.Key.ToString(), kv.Value + " ms");
                }
            }
        }
    }
}
