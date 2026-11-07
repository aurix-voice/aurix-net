// Project-wide Aurix Net settings: create via Assets > Create > Aurix Net > Settings.
using UnityEngine;

namespace Aurix.Net.Unity
{
    [CreateAssetMenu(menuName = "Aurix Net/Settings", fileName = "AurixNetSettings")]
    public sealed class NetSettings : ScriptableObject
    {
        [Tooltip("Node or cloud URL, e.g. https://eu.net.aurix.dev")] public string ServerUrl = "http://127.0.0.1:8090";
        [Tooltip("Dev only: issues grants from the client with an API key. Never ship a key; in production your backend calls POST /v1/grants.")]
        public string DevApiKey = "";
        public RoomMode DefaultMode = RoomMode.Sync;
        public int TickHz = 60;
        public int MaxPlayers = 8;
        [Tooltip("IDL schema for sync rooms (components and fields). Empty = built-in arena schema.")]
        [TextArea(6, 30)] public string Schema = "component Transform {\n  pos: vec3 @range(-1024, 1024, 0.01)\n  rot: quat\n}\ncomponent Prefab {\n  name: string(32)\n}\n";
        [Tooltip("Remote entities are rendered this many ticks behind the server for smooth interpolation.")]
        public int InterpolationTicks = 2;
        [Tooltip("Prefabs that can be spawned for replicated entities; matched by the Prefab.name component, the first one is the default.")]
        public NetView[] Prefabs = new NetView[0];
    }
}
