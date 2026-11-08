// The one component a scene needs: connects, joins, spawns prefabs for replicated entities,
// sends the local player's [Sync] writes as inputs every tick, dispatches events and RPCs.
using System;
using System.Collections.Generic;
using System.Text;
using System.Threading.Tasks;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity
{
    [DefaultExecutionOrder(-100)]
    public sealed class NetRunner : MonoBehaviour
    {
        public static NetRunner Instance { get; private set; }
        public NetSettings Settings;
        [Tooltip("Join this room on Start (empty = call JoinOrCreate yourself).")] public string AutoJoinRoom = "";
        [Tooltip("Player id for dev grants (DevApiKey); production grants come from your backend.")] public string DevPlayerId = "";

        public ManagedClient Client { get; private set; }
        public Schema Schema { get; private set; }
        public SyncWorld World { get; private set; }
        /// <summary>Lockstep rooms: confirmed inputs per tick, catch-up, hashes (see LockstepClient).</summary>
        public LockstepClient Lockstep { get; private set; }
        public int Actor => Client?.Actor ?? -1;
        public RoomMode Mode { get; private set; }
        public bool IsJoined { get; private set; }
        public uint ServerTick => World?.ServerTick ?? 0;
        /// <summary>Server tick remote entities are rendered at (ServerTick - InterpolationTicks, advanced between deltas).</summary>
        public float RenderTick { get; private set; }
        public readonly Dictionary<ulong, NetView> Views = new Dictionary<ulong, NetView>();

        public event Action OnJoined;
        public event Action<string> OnClosed;
        public event Action<EventFrame> OnEvent;
        public event Action<string> OnControl;
        public event Action<NetView> OnSpawned, OnDespawned;
        /// <summary>Server state for an entity you own arrived (tick = server tick). Feed it to a
        /// <see cref="Predictor{TState}"/> to reconcile when your WASM logic drives the entity.</summary>
        public event Action<NetView, NetEntity, uint> OnOwnEntityUpdated;
        /// <summary>Tick stamped on the last input sent (what to pass to <c>Predictor.Record</c>).</summary>
        public uint LastInputTick => _lastInputTick;

        const byte RpcEventCode = 0xF0;
        readonly List<(ComponentDef, ComponentValue)> _writes = new List<(ComponentDef, ComponentValue)>();
        readonly List<byte[]> _recentInputs = new List<byte[]>();
        uint _lastInputTick; float _tickTimer; float _lastDeltaTime;

        void Awake() { Instance = this; }

        async void Start()
        {
            if (!string.IsNullOrEmpty(AutoJoinRoom))
            {
                try { await ConnectWithDevGrantAsync(); await JoinOrCreateAsync(AutoJoinRoom); }
                catch (Exception e) { Debug.LogError("Aurix Net: " + e.Message); }
            }
        }

        /// <summary>Dev only: issues a grant with Settings.DevApiKey and connects.</summary>
        public async Task ConnectWithDevGrantAsync()
        {
            if (string.IsNullOrEmpty(Settings.DevApiKey)) throw new NetException("NetSettings.DevApiKey is empty; pass a grant from your backend to ConnectAsync instead");
            var pid = string.IsNullOrEmpty(DevPlayerId) ? "dev-" + Guid.NewGuid().ToString("N").Substring(0, 8) : DevPlayerId;
            await ConnectAsync(await DevGrant.IssueAsync(Settings.ServerUrl, Settings.DevApiKey, pid, pid));
        }

        /// <summary>Connects with a grant your backend issued (POST /v1/grants).</summary>
        public async Task ConnectAsync(string grant)
        {
            Client = await ManagedClient.ConnectAsync(Settings.ServerUrl, grant);
            Client.OnControl += j => OnControl?.Invoke(j);
            Client.OnEvent += HandleEvent;
            Client.OnData += HandleData;
            Client.OnClosed += r => { IsJoined = false; OnClosed?.Invoke(r); };
            Client.OnRehost += () => _ = RejoinAsync();
        }

        [Tooltip("Other nodes/region URLs to try when the entry node is gone during a rehost.")] public string[] FallbackUrls = new string[0];

        /// <summary>Rehost (node shut down, room handed to another node): drop the views, rejoin the same room.</summary>
        public async Task RejoinAsync()
        {
            IsJoined = false;
            foreach (var v in Views.Values) Destroy(v.gameObject);
            Views.Clear(); World = null; Lockstep = null;
            try
            {
                var old = Client; Client = null;
                var c = await old.RejoinAsync(FallbackUrls);
                Client = c;
                c.OnControl += j => OnControl?.Invoke(j);
                c.OnEvent += HandleEvent; c.OnData += HandleData;
                c.OnClosed += r => { IsJoined = false; OnClosed?.Invoke(r); };
                c.OnRehost += () => _ = RejoinAsync();
                Mode = Enum.TryParse<RoomMode>(c.RoomMode, true, out var m) ? m : Mode;
                AfterJoin();
            }
            catch (Exception e) { Debug.LogError("Aurix Net rehost failed: " + e.Message); OnClosed?.Invoke("rehost failed: " + e.Message); }
        }

        public Task JoinOrCreateAsync(string room) => JoinOrCreateAsync(room, Settings.DefaultMode);
        public async Task JoinOrCreateAsync(string room, RoomMode mode, int maxPlayers = 0)
        {
            Mode = mode;
            var props = new StringBuilder("{\"tick_hz\":").Append(Settings.TickHz);
            if (mode == RoomMode.Sync && !string.IsNullOrEmpty(Settings.Schema)) props.Append(",\"schema\":").Append(JsonStr(Settings.Schema));
            props.Append('}');
            await Client.JoinOrCreateAsync(room, mode.ToString().ToLowerInvariant(), maxPlayers > 0 ? maxPlayers : Settings.MaxPlayers, props.ToString());
            AfterJoin();
        }
        public async Task JoinAsync(string room, bool spectator = false)
        {
            await Client.JoinAsync(room, null, spectator);
            Mode = Enum.TryParse<RoomMode>(Client.RoomMode, true, out var m) ? m : RoomMode.Relay;
            AfterJoin();
        }
        void AfterJoin()
        {
            if (Mode == RoomMode.Sync)
            {
                Schema = Schema.Parse(string.IsNullOrEmpty(Settings.Schema) ? ArenaSchema : Settings.Schema);
                World = new SyncWorld(Schema);
                World.OnSpawn += Spawn; World.OnChange += Change; World.OnDespawn += Despawn;
            }
            if (Mode == RoomMode.Lockstep) Lockstep = new LockstepClient((ch, b) => Client.SendData(ch, b));
            IsJoined = true; OnJoined?.Invoke();
        }

        public const string ArenaSchema = "component Transform {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n}\n";

        void HandleData(Channel ch, ushort tick, byte[] p)
        {
            if (Lockstep != null) { Lockstep.Receive(p); return; }
            if (World == null) return;
            var ack = World.Receive(p);
            if (ack != null) { Client.SendData(Channel.Unreliable, ack); RenderTick = Mathf.Max(RenderTick, World.ServerTick - Settings.InterpolationTicks); _lastDeltaTime = Time.unscaledTime; }
        }

        void Spawn(NetEntity e)
        {
            var prefab = PickPrefab(e); if (prefab == null) return;
            var view = Instantiate(prefab); view.name = prefab.name + "#" + e.Id;
            Views[e.Id] = view; view.Bind(this, e);
            _spawned.Clear(); view.GetComponentsInChildren(true, _spawned); foreach (var s in _spawned) s.Spawned(view);
            OnSpawned?.Invoke(view);
        }
        readonly List<ISpawned> _spawned = new List<ISpawned>(); readonly List<IDespawned> _despawned = new List<IDespawned>();

        /// <summary>Timer expiring <paramref name="seconds"/> of server ticks from now (same on every peer).</summary>
        public TickTimer TimerFromSeconds(float seconds) => TickTimer.CreateFromSeconds(ServerTick, seconds, Settings.TickHz);
        public TickTimer TimerFromTicks(uint ticks) => TickTimer.CreateFromTicks(ServerTick, ticks);
        public bool Expired(TickTimer t) => t.Expired(ServerTick);
        public float RemainingSeconds(TickTimer t) => t.RemainingSeconds(ServerTick, Settings.TickHz);
        void Change(NetEntity e)
        {
            if (!Views.TryGetValue(e.Id, out var v)) { Spawn(e); return; }
            v.Owner = e.Owner;
            if (v.IsMine) { OnOwnEntityUpdated?.Invoke(v, e, World.ServerTick); return; }
            foreach (var b in v.Behaviours) b.ApplyFromEntity(e, World.ServerTick);
        }
        void Despawn(NetEntity e)
        {
            if (!Views.TryGetValue(e.Id, out var v)) return;
            Views.Remove(e.Id);
            _despawned.Clear(); v.GetComponentsInChildren(true, _despawned); foreach (var d in _despawned) d.Despawned(v);
            OnDespawned?.Invoke(v); Destroy(v.gameObject);
        }
        NetView PickPrefab(NetEntity e)
        {
            if (Settings.Prefabs == null || Settings.Prefabs.Length == 0) return null;
            var pc = Schema.ByName("Prefab"); var pv = pc != null ? e.Get(pc) : null;
            if (pv != null && pc.IndexOf("name") >= 0) { var n = pv.Fields[pc.IndexOf("name")].S; foreach (var p in Settings.Prefabs) if (p != null && p.name == n) return p; }
            return Settings.Prefabs[0];
        }

        void Update()
        {
            Client?.Poll();
            if (World != null && Settings.TickHz > 0) RenderTick += Time.unscaledDeltaTime * Settings.TickHz; // advance between deltas
        }

        void LateUpdate()
        {
            if (Client == null || !IsJoined) return;
            if (World != null && Settings.TickHz > 0)
            {
                _tickTimer += Time.unscaledDeltaTime; float dt = 1f / Settings.TickHz;
                if (_tickTimer >= dt) { _tickTimer -= dt; if (_tickTimer > dt) _tickTimer = 0; SendOwnerInputs(); }
            }
            Client.Flush();
        }

        void SendOwnerInputs()
        {
            _writes.Clear();
            foreach (var v in Views.Values) if (v.IsMine) foreach (var b in v.Behaviours) { var w = b.CollectWrite(false); if (w.HasValue) _writes.Add(w.Value); }
            uint tick = Math.Max(World.ServerTick + 2, _lastInputTick + 1); _lastInputTick = tick;
            var body = _writes.Count > 0 ? World.EncodeWrites(_writes) : Array.Empty<byte>();
            _recentInputs.Add(body); while (_recentInputs.Count > 3) _recentInputs.RemoveAt(0);
            if (_writes.Count == 0 && _recentInputs.TrueForAll(b => b.Length == 0)) return;
            Client.SendData(Channel.Unreliable, SyncWorld.EncodeInput(tick - (uint)(_recentInputs.Count - 1), _recentInputs));
        }

        /// <summary>Sends a raw input frame for the current tick to the server logic (`props.logic`); returns the
        /// tick it was stamped with. Resent with the next two inputs for loss tolerance.</summary>
        public uint SendInput(byte[] body)
        {
            if (World == null || Client == null) return 0;
            uint tick = Math.Max(World.ServerTick + 2, _lastInputTick + 1); _lastInputTick = tick;
            _recentInputs.Add(body ?? Array.Empty<byte>()); while (_recentInputs.Count > 3) _recentInputs.RemoveAt(0);
            Client.SendData(Channel.Unreliable, SyncWorld.EncodeInput(tick - (uint)(_recentInputs.Count - 1), _recentInputs));
            return tick;
        }

        // ---- events / RPC (relay channel) ----
        public void SendEvent(byte code, byte[] payload, Target target = Target.Others, bool reliable = false) => Client.SendEvent(code, payload, target, reliable);

        internal void SendRpc(NetView view, string method, object[] args)
        {
            var w = new BitWriter(); w.WriteBits(view.EntityId, 48);
            var mb = Encoding.UTF8.GetBytes(method); w.WriteBits((ulong)mb.Length, 8); w.WriteBytes(mb);
            w.WriteBits((ulong)args.Length, 8);
            foreach (var a in args) RpcArgs.Write(w, a);
            Client.SendEvent(RpcEventCode, w.Finish(), Target.Others, reliable: true);
        }
        void HandleEvent(EventFrame f)
        {
            if (f.Code != RpcEventCode) { OnEvent?.Invoke(f); return; }
            var buf = new byte[f.Payload.Count]; Array.Copy(f.Payload.Array, f.Payload.Offset, buf, 0, buf.Length);
            var r = new BitReader(buf); ulong id = r.ReadBits(48);
            var method = Encoding.UTF8.GetString(r.ReadBytes((int)r.ReadBits(8))); int n = (int)r.ReadBits(8);
            if (!Views.TryGetValue(id, out var v)) return;
            foreach (var b in v.Behaviours)
            {
                var m = b.RpcMethod(method); if (m == null) continue;
                var ps = m.GetParameters(); var args = new object[n];
                for (int i = 0; i < n; i++) args[i] = RpcArgs.Read(r, i < ps.Length ? ps[i].ParameterType : typeof(object));
                b.InvokeRpc(method, args); return;
            }
        }

        static string JsonStr(string s)
        {
            var sb = new StringBuilder("\"");
            foreach (var c in s) { if (c == '"' || c == '\\') sb.Append('\\').Append(c); else if (c == '\n') sb.Append("\\n"); else if (c < 0x20) sb.Append("\\u").Append(((int)c).ToString("x4")); else sb.Append(c); }
            return sb.Append('"').ToString();
        }

        void OnDestroy() { Client?.Dispose(); if (Instance == this) Instance = null; }
    }

    /// <summary>Tagged RPC argument codec: bool, int, long, float, string, byte[], Vector3, Quaternion.</summary>
    static class RpcArgs
    {
        public static void Write(BitWriter w, object a)
        {
            switch (a)
            {
                case bool b: w.WriteBits(1, 4); w.WriteBool(b); break;
                case int i: w.WriteBits(2, 4); w.WriteInt(i, 32); break;
                case long l: w.WriteBits(3, 4); w.WriteInt(l, 64); break;
                case float f: w.WriteBits(4, 4); w.WriteF32(f); break;
                case string s: { w.WriteBits(5, 4); var b = Encoding.UTF8.GetBytes(s); w.WriteBits((ulong)b.Length, 16); w.WriteBytes(b); break; }
                case byte[] bs: w.WriteBits(6, 4); w.WriteBits((ulong)bs.Length, 16); w.WriteBytes(bs); break;
                case Vector3 v: w.WriteBits(7, 4); w.WriteF32(v.x); w.WriteF32(v.y); w.WriteF32(v.z); break;
                case Quaternion q: w.WriteBits(8, 4); w.WriteF32(q.x); w.WriteF32(q.y); w.WriteF32(q.z); w.WriteF32(q.w); break;
                default: throw new NetException("unsupported RPC argument type " + a?.GetType().Name);
            }
        }
        public static object Read(BitReader r, Type want)
        {
            switch ((int)r.ReadBits(4))
            {
                case 1: return r.ReadBool();
                case 2: return Convert.ChangeType((int)r.ReadInt(32), want == typeof(object) ? typeof(int) : want);
                case 3: return r.ReadInt(64);
                case 4: return Convert.ChangeType(r.ReadF32(), want == typeof(object) ? typeof(float) : want);
                case 5: return Encoding.UTF8.GetString(r.ReadBytes((int)r.ReadBits(16)));
                case 6: return r.ReadBytes((int)r.ReadBits(16));
                case 7: return new Vector3(r.ReadF32(), r.ReadF32(), r.ReadF32());
                case 8: return new Quaternion(r.ReadF32(), r.ReadF32(), r.ReadF32(), r.ReadF32());
            }
            throw new NetException("bad RPC argument");
        }
    }

    /// <summary>Dev-time grant issuance from the client. Production: your backend calls POST /v1/grants.</summary>
    public static class DevGrant
    {
        public static async Task<string> IssueAsync(string http, string apiKey, string playerId, string name)
        {
            using (var c = new System.Net.Http.HttpClient())
            {
                var req = new System.Net.Http.HttpRequestMessage(System.Net.Http.HttpMethod.Post, http.TrimEnd('/') + "/v1/grants");
                req.Headers.Authorization = new System.Net.Http.Headers.AuthenticationHeaderValue("Bearer", apiKey);
                req.Content = new System.Net.Http.StringContent("{\"player_id\":" + Q(playerId) + ",\"name\":" + Q(name) + "}", Encoding.UTF8, "application/json");
                var r = await c.SendAsync(req); var body = await r.Content.ReadAsStringAsync();
                if (!r.IsSuccessStatusCode) throw new NetException("grant: " + body);
                return ManagedClientTestHooks.Str(body, "grant");
            }
        }
        static string Q(string s) => "\"" + s.Replace("\\", "\\\\").Replace("\"", "\\\"") + "\"";
    }
}
