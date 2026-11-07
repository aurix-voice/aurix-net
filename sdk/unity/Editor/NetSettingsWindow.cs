// Window > Aurix Net: finds or creates the NetSettings asset and edits it in place.
using System.Net.Http;
using UnityEditor;
using UnityEngine;

namespace Aurix.Net.Unity.Editor
{
    public sealed class NetSettingsWindow : EditorWindow
    {
        NetSettings _settings; UnityEditor.Editor _editor; string _status = "";

        [MenuItem("Window/Aurix Net")]
        public static void Open() => GetWindow<NetSettingsWindow>("Aurix Net");

        void OnEnable() { _settings = Find(); if (_settings != null) _editor = UnityEditor.Editor.CreateEditor(_settings); }

        static NetSettings Find()
        {
            foreach (var g in AssetDatabase.FindAssets("t:NetSettings")) return AssetDatabase.LoadAssetAtPath<NetSettings>(AssetDatabase.GUIDToAssetPath(g));
            return null;
        }

        void OnGUI()
        {
            if (_settings == null)
            {
                EditorGUILayout.HelpBox("No NetSettings asset yet.", MessageType.Info);
                if (GUILayout.Button("Create Assets/AurixNetSettings.asset"))
                {
                    _settings = CreateInstance<NetSettings>();
                    AssetDatabase.CreateAsset(_settings, "Assets/AurixNetSettings.asset"); AssetDatabase.SaveAssets();
                    _editor = UnityEditor.Editor.CreateEditor(_settings);
                }
                return;
            }
            _editor.OnInspectorGUI();
            GUILayout.Space(8);
            if (GUILayout.Button("Test connection")) _ = Test();
            if (!string.IsNullOrEmpty(_status)) EditorGUILayout.HelpBox(_status, MessageType.None);
            GUILayout.Space(8);
            EditorGUILayout.HelpBox("Scene: add a NetRunner, assign this asset, set AutoJoinRoom. Prefab: NetView + NetTransform (+ your SyncBehaviours), add it to Prefabs. Production grants come from your backend (POST /v1/grants); DevApiKey is for the editor only.", MessageType.Info);
        }

        async System.Threading.Tasks.Task Test()
        {
            try
            {
                using (var c = new HttpClient()) { var r = await c.GetAsync(_settings.ServerUrl.TrimEnd('/') + "/healthz"); _status = r.IsSuccessStatusCode ? "Node reachable: " + _settings.ServerUrl : "HTTP " + (int)r.StatusCode; }
            }
            catch (System.Exception e) { _status = "Unreachable: " + e.Message; }
            Repaint();
        }
    }
}
