// Aurix Net Unity SDK — editor window showing live NetworkStatistics of the running NetworkManager.
// SPDX-License-Identifier: Apache-2.0
using UnityEditor;
using UnityEngine;
using Aurix.Net.Unity.Framework;

namespace Aurix.Net.Unity.Editor
{
    public sealed class NetworkStatisticsWindow : EditorWindow
    {
        [MenuItem("Window/Aurix Net/Network Statistics")]
        static void Open() => GetWindow<NetworkStatisticsWindow>("Aurix Net Stats");

        void OnInspectorUpdate() => Repaint();

        void OnGUI()
        {
            var m = NetworkManager.main;
            if (m == null || (!m.isServer && !m.isClient)) { EditorGUILayout.HelpBox("No running NetworkManager (enter play mode and start host/server/client).", MessageType.Info); return; }
            var s = m.statistics;
            EditorGUILayout.LabelField("Role", m.isHost ? "Host" : m.isServer ? "Server" : "Client");
            EditorGUILayout.LabelField("Server tick", m.serverTick.ToString());
            EditorGUILayout.LabelField("Players", m.players.Count.ToString());
            EditorGUILayout.LabelField("Objects", m.identities is System.Collections.Generic.ICollection<NetworkIdentity> c ? c.Count.ToString() : "-");
            EditorGUILayout.Space();
            if (m.isClient && !m.isServer)
            {
                EditorGUILayout.LabelField("Ping", s.hasPingEstimate ? $"{s.ping} ms (last {s.lastPing}, jitter {s.jitter})" : "measuring…");
                if (s.isHighPing) EditorGUILayout.HelpBox("High ping", MessageType.Warning);
                if (s.isHighJitter) EditorGUILayout.HelpBox("High jitter", MessageType.Warning);
            }
            if (s.isConnectionStalled) EditorGUILayout.HelpBox($"Connection stalled: nothing received for {s.secondsSinceLastReceived:0.0}s", MessageType.Error);
            EditorGUILayout.LabelField("Upload", $"{s.upload / 1024f:0.0} KB/s, {s.messagesOutPerSecond} msg/s");
            EditorGUILayout.LabelField("Download", $"{s.download / 1024f:0.0} KB/s, {s.messagesInPerSecond} msg/s");
            EditorGUILayout.LabelField("Total", $"{s.totalBytesOut / 1024f:0.0} KB out, {s.totalBytesIn / 1024f:0.0} KB in");
            if (m.isServer && s.playerPings.Count > 0)
            {
                EditorGUILayout.Space(); EditorGUILayout.LabelField("Player pings", EditorStyles.boldLabel);
                foreach (var kv in s.playerPings) EditorGUILayout.LabelField(kv.Key.ToString(), kv.Value + " ms");
            }
        }
    }
}
