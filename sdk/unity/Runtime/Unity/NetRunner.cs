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

        [Tooltip("Join this room on Start (empty = call JoinOrCreate yourself).")]
        public string AutoJoinRoom = "";

        [Tooltip("Player id for dev grants (DevApiKey); production grants come from your backend.")]
        public string DevPlayerId = "";

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
        public event Action<NetView> OnSpawned,
            OnDespawned;

        /// <summary>Server state for an entity you own arrived (tick = server tick). Feed it to a
        /// <see cref="Predictor{TState}"/> to reconcile when your WASM logic drives the entity.</summary>
        public event Action<NetView, NetEntity, uint> OnOwnEntityUpdated;

        /// <summary>Tick stamped on the last input sent (what to pass to <c>Predictor.Record</c>).</summary>
        public uint LastInputTick => _lastInputTick;

        const byte RpcEventCode = 0xF0;
        readonly List<(ComponentDef, ComponentValue)> _writes =
            new List<(ComponentDef, ComponentValue)>();
        readonly List<byte[]> _recentInputs = new List<byte[]>();
        uint _lastInputTick;

        // The room drops an input whose tick it has already simulated, and one more than 64 ticks ahead.
        // A frame carries inputs for consecutive ticks, so earlier ones are resent only without a gap.
        // The round trip gives the first guess; the room's margin reports (0x25) correct it.
        uint InputLead
        {
            get
            {
                int estimate =
                    2 + Math.Min(60, (int)Math.Ceiling(Client.RttMs * Settings.TickHz / 1000f));
                int lead = Math.Min(62, Math.Max(1, estimate + _leadAdjust));
                _leadAdjust = lead - estimate;
                return (uint)lead;
            }
        }

        readonly MarginControl _margin = new MarginControl();
        int _leadAdjust;
        float _tickTimer;
        float _lastDeltaTime;
        bool _synced;
        byte[] _heldInput;

        void Awake()
        {
            Instance = this;
        }

        async void Start()
        {
            if (!string.IsNullOrEmpty(AutoJoinRoom))
            {
                try
                {
                    await ConnectWithDevGrantAsync();
                    await JoinOrCreateAsync(AutoJoinRoom);
                }
                catch (Exception error)
                {
                    Debug.LogError("Aurix Net: " + error.Message);
                }
            }
        }

        /// <summary>Dev only: issues a grant with Settings.DevApiKey and connects.</summary>
        public async Task ConnectWithDevGrantAsync()
        {
            if (string.IsNullOrEmpty(Settings.DevApiKey))
            {
                throw new NetException(
                    "NetSettings.DevApiKey is empty; pass a grant from your backend to ConnectAsync instead"
                );
            }

            var pid = string.IsNullOrEmpty(DevPlayerId)
                ? "dev-" + Guid.NewGuid().ToString("N").Substring(0, 8)
                : DevPlayerId;
            await ConnectAsync(
                await DevGrant.IssueAsync(Settings.ServerUrl, Settings.DevApiKey, pid, pid)
            );
        }

        /// <summary>Connects with a grant your backend issued (POST /v1/grants).</summary>
        public async Task ConnectAsync(string grant)
        {
            Client = await ManagedClient.ConnectAsync(Settings.ServerUrl, grant);
            Client.OnControl += json => OnControl?.Invoke(json);
            Client.OnEvent += HandleEvent;
            Client.OnData += HandleData;
            Client.OnClosed += result =>
            {
                IsJoined = false;
                OnClosed?.Invoke(result);
            };
            Client.OnRehost += () => _ = RejoinAsync();
        }

        [Tooltip("Other nodes/region URLs to try when the entry node is gone during a rehost.")]
        public string[] FallbackUrls = new string[0];

        /// <summary>Rehost (node shut down, room handed to another node): drop the views, rejoin the same room.</summary>
        public async Task RejoinAsync()
        {
            IsJoined = false;
            foreach (var view in Views.Values)
            {
                Destroy(view.gameObject);
            }

            Views.Clear();
            World = null;
            Lockstep = null;
            try
            {
                var old = Client;
                Client = null;
                var client = await old.RejoinAsync(FallbackUrls);
                Client = client;
                client.OnControl += json => OnControl?.Invoke(json);
                client.OnEvent += HandleEvent;
                client.OnData += HandleData;
                client.OnClosed += result =>
                {
                    IsJoined = false;
                    OnClosed?.Invoke(result);
                };
                client.OnRehost += () => _ = RejoinAsync();
                Mode = Enum.TryParse<RoomMode>(client.RoomMode, true, out var mode) ? mode : Mode;
                AfterJoin();
            }
            catch (Exception error)
            {
                Debug.LogError("Aurix Net rehost failed: " + error.Message);
                OnClosed?.Invoke("rehost failed: " + error.Message);
            }
        }

        public Task JoinOrCreateAsync(string room) => JoinOrCreateAsync(room, Settings.DefaultMode);

        public async Task JoinOrCreateAsync(string room, RoomMode mode, int maxPlayers = 0)
        {
            Mode = mode;
            var props = new StringBuilder("{\"tick_hz\":").Append(Settings.TickHz);
            if (mode == RoomMode.Sync && !string.IsNullOrEmpty(Settings.Schema))
            {
                props.Append(",\"schema\":").Append(JsonStr(Settings.Schema));
            }

            props.Append('}');
            await Client.JoinOrCreateAsync(
                room,
                mode.ToString().ToLowerInvariant(),
                maxPlayers > 0 ? maxPlayers : Settings.MaxPlayers,
                props.ToString()
            );
            AfterJoin();
        }

        public async Task JoinAsync(string room, bool spectator = false)
        {
            await Client.JoinAsync(room, null, spectator);
            Mode = Enum.TryParse<RoomMode>(Client.RoomMode, true, out var mode)
                ? mode
                : RoomMode.Relay;
            AfterJoin();
        }

        void AfterJoin()
        {
            _lastInputTick = 0;
            _synced = false;
            _recentInputs.Clear();
            _heldInput = null; // input ticks belong to the room just left
            if (Mode == RoomMode.Sync)
            {
                Schema = Schema.Parse(
                    string.IsNullOrEmpty(Settings.Schema) ? ArenaSchema : Settings.Schema
                );
                World = new SyncWorld(Schema);
                World.OnSpawn += Spawn;
                World.OnChange += Change;
                World.OnDespawn += Despawn;
            }
            if (Mode == RoomMode.Lockstep)
            {
                Lockstep = new LockstepClient((ch, bytes) => Client.SendData(ch, bytes));
            }

            IsJoined = true;
            OnJoined?.Invoke();
        }

        public const string ArenaSchema =
            "component Transform {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n}\n";

        void HandleData(Channel ch, ushort tick, byte[] payload)
        {
            if (Lockstep != null)
            {
                Lockstep.Receive(payload);
                return;
            }
            if (World == null)
            {
                return;
            }

            if (payload.Length >= 7 && payload[0] == 0x25)
            {
                uint reportTick =
                    ((uint)payload[1] << 24)
                    | ((uint)payload[2] << 16)
                    | ((uint)payload[3] << 8)
                    | payload[4];
                _leadAdjust += _margin.Adjust(reportTick, (sbyte)payload[5], _lastInputTick);
                return;
            }

            var ack = World.Receive(payload);
            if (ack != null)
            {
                Client.SendData(Channel.Unreliable, ack);
                RenderTick = Mathf.Max(RenderTick, World.ServerTick - Settings.InterpolationTicks);
                _lastDeltaTime = Time.unscaledTime;
                _synced = true;
            }
        }

        void Spawn(NetEntity entity)
        {
            var prefab = PickPrefab(entity);
            if (prefab == null)
            {
                return;
            }

            var view = Instantiate(prefab);
            view.name = prefab.name + "#" + entity.Id;
            Views[entity.Id] = view;
            view.Bind(this, entity);
            _spawned.Clear();
            view.GetComponentsInChildren(true, _spawned);
            foreach (var spawned in _spawned)
            {
                spawned.Spawned(view);
            }

            OnSpawned?.Invoke(view);
        }

        readonly List<ISpawned> _spawned = new List<ISpawned>();
        readonly List<IDespawned> _despawned = new List<IDespawned>();

        /// <summary>Timer expiring <paramref name="seconds"/> of server ticks from now (same on every peer).</summary>
        public TickTimer TimerFromSeconds(float seconds) =>
            TickTimer.CreateFromSeconds(ServerTick, seconds, Settings.TickHz);

        public TickTimer TimerFromTicks(uint ticks) => TickTimer.CreateFromTicks(ServerTick, ticks);

        public bool Expired(TickTimer timer) => timer.Expired(ServerTick);

        public float RemainingSeconds(TickTimer timer) =>
            timer.RemainingSeconds(ServerTick, Settings.TickHz);

        void Change(NetEntity entity)
        {
            if (!Views.TryGetValue(entity.Id, out var view))
            {
                Spawn(entity);
                return;
            }
            view.Owner = entity.Owner;
            if (view.IsMine)
            {
                OnOwnEntityUpdated?.Invoke(view, entity, World.ServerTick);
                return;
            }
            foreach (var behaviour in view.Behaviours)
            {
                behaviour.ApplyFromEntity(entity, World.ServerTick);
            }
        }

        void Despawn(NetEntity entity)
        {
            if (!Views.TryGetValue(entity.Id, out var view))
            {
                return;
            }

            Views.Remove(entity.Id);
            _despawned.Clear();
            view.GetComponentsInChildren(true, _despawned);
            foreach (var despawned in _despawned)
            {
                despawned.Despawned(view);
            }

            OnDespawned?.Invoke(view);
            Destroy(view.gameObject);
        }

        NetView PickPrefab(NetEntity entity)
        {
            if (Settings.Prefabs == null || Settings.Prefabs.Length == 0)
            {
                return null;
            }

            var pc = Schema.ByName("Prefab");
            var pv = pc != null ? entity.Get(pc) : null;
            if (pv != null && pc.IndexOf("name") >= 0)
            {
                var name = pv.Fields[pc.IndexOf("name")].S;
                foreach (var prefab in Settings.Prefabs)
                {
                    if (prefab != null && prefab.name == name)
                    {
                        return prefab;
                    }
                }
            }
            return Settings.Prefabs[0];
        }

        void Update()
        {
            Client?.Poll();
            if (World != null && Settings.TickHz > 0)
            {
                RenderTick += Time.unscaledDeltaTime * Settings.TickHz; // advance between deltas
            }
        }

        void LateUpdate()
        {
            if (Client == null || !IsJoined)
            {
                return;
            }

            if (World != null)
            {
                SendHeldInput();
            }

            if (World != null && Settings.TickHz > 0)
            {
                _tickTimer += Time.unscaledDeltaTime;
                float dt = 1f / Settings.TickHz;
                if (_tickTimer >= dt)
                {
                    _tickTimer -= dt;
                    if (_tickTimer > dt)
                    {
                        _tickTimer = 0;
                    }

                    SendOwnerInputs();
                }
            }
            Client.Flush();
        }

        void SendOwnerInputs()
        {
            _writes.Clear();
            foreach (var view in Views.Values)
            {
                if (view.IsMine)
                {
                    foreach (var behaviour in view.Behaviours)
                    {
                        var write = behaviour.CollectWrite(false);
                        if (write.HasValue)
                        {
                            _writes.Add(write.Value);
                        }
                    }
                }
            }

            uint tick = Math.Max(World.ServerTick + InputLead, _lastInputTick + 1);
            if (tick != _lastInputTick + 1)
            {
                _recentInputs.Clear();
            }

            _lastInputTick = tick;
            var body = _writes.Count > 0 ? World.EncodeWrites(_writes) : Array.Empty<byte>();
            _recentInputs.Add(body);
            while (_recentInputs.Count > 3)
            {
                _recentInputs.RemoveAt(0);
            }

            if (_writes.Count == 0 && _recentInputs.TrueForAll(bytes => bytes.Length == 0))
            {
                return;
            }

            Client.SendData(
                Channel.Unreliable,
                SyncWorld.EncodeInput(tick - (uint)(_recentInputs.Count - 1), _recentInputs)
            );
        }

        /// <summary>Sends a raw input frame for the current tick to the server logic (`props.logic`); returns the
        /// tick it is stamped with. Resent with the next two inputs for loss tolerance. The room takes one input
        /// per tick: call this every frame, the newest input of a tick is the one sent.</summary>
        public uint SendInput(byte[] body)
        {
            if (World == null || Client == null)
            {
                return 0;
            }

            _heldInput = body ?? Array.Empty<byte>();
            return SendHeldInput() ? _lastInputTick : _lastInputTick + 1;
        }

        // An input stamped far ahead of the room is applied that much later, so it waits for the room instead,
        // and for the first delta, which tells the room's tick.
        bool SendHeldInput()
        {
            if (_heldInput == null || !_synced)
            {
                return false;
            }

            uint lead = InputLead;
            uint tick = Math.Max(World.ServerTick + lead, _lastInputTick + 1);
            if (tick > World.ServerTick + lead + 1)
            {
                return false;
            }

            if (tick != _lastInputTick + 1)
            {
                _recentInputs.Clear();
            }

            _lastInputTick = tick;
            _recentInputs.Add(_heldInput);
            _heldInput = null;
            while (_recentInputs.Count > 3)
            {
                _recentInputs.RemoveAt(0);
            }

            Client.SendData(
                Channel.Unreliable,
                SyncWorld.EncodeInput(tick - (uint)(_recentInputs.Count - 1), _recentInputs)
            );
            return true;
        }

        // ---- events / RPC (relay channel) ----
        public void SendEvent(
            byte code,
            byte[] payload,
            Target target = Target.Others,
            bool reliable = false
        ) => Client.SendEvent(code, payload, target, reliable);

        internal void SendRpc(NetView view, string method, object[] args)
        {
            var writer = new BitWriter();
            writer.WriteBits(view.EntityId, 48);
            var mb = Encoding.UTF8.GetBytes(method);
            writer.WriteBits((ulong)mb.Length, 8);
            writer.WriteBytes(mb);
            writer.WriteBits((ulong)args.Length, 8);
            foreach (var argument in args)
            {
                RpcArgs.Write(writer, argument);
            }

            Client.SendEvent(RpcEventCode, writer.Finish(), Target.Others, reliable: true);
        }

        void HandleEvent(EventFrame frame)
        {
            if (frame.Code != RpcEventCode)
            {
                OnEvent?.Invoke(frame);
                return;
            }
            var buf = new byte[frame.Payload.Count];
            Array.Copy(frame.Payload.Array, frame.Payload.Offset, buf, 0, buf.Length);
            var reader = new BitReader(buf);
            ulong id = reader.ReadBits(48);
            var method = Encoding.UTF8.GetString(reader.ReadBytes((int)reader.ReadBits(8)));
            int count = (int)reader.ReadBits(8);
            if (!Views.TryGetValue(id, out var view))
            {
                return;
            }

            foreach (var behaviour in view.Behaviours)
            {
                var rpc = behaviour.RpcMethod(method);
                if (rpc == null)
                {
                    continue;
                }

                var ps = rpc.GetParameters();
                var args = new object[count];
                for (int i = 0; i < count; i++)
                {
                    args[i] = RpcArgs.Read(
                        reader,
                        i < ps.Length ? ps[i].ParameterType : typeof(object)
                    );
                }

                behaviour.InvokeRpc(method, args);
                return;
            }
        }

        static string JsonStr(string text)
        {
            var sb = new StringBuilder("\"");
            foreach (var character in text)
            {
                if (character == '"' || character == '\\')
                {
                    sb.Append('\\').Append(character);
                }
                else if (character == '\n')
                {
                    sb.Append("\\n");
                }
                else if (character < 0x20)
                {
                    sb.Append("\\u").Append(((int)character).ToString("x4"));
                }
                else
                {
                    sb.Append(character);
                }
            }
            return sb.Append('"').ToString();
        }

        void OnDestroy()
        {
            Client?.Dispose();
            if (Instance == this)
            {
                Instance = null;
            }
        }
    }

    /// <summary>Tagged RPC argument codec: bool, int, long, float, string, byte[], Vector3, Quaternion.</summary>
    static class RpcArgs
    {
        public static void Write(BitWriter writer, object argument)
        {
            switch (argument)
            {
                case bool flag:
                    writer.WriteBits(1, 4);
                    writer.WriteBool(flag);
                    break;
                case int number:
                    writer.WriteBits(2, 4);
                    writer.WriteInt(number, 32);
                    break;
                case long number:
                    writer.WriteBits(3, 4);
                    writer.WriteInt(number, 64);
                    break;
                case float number:
                    writer.WriteBits(4, 4);
                    writer.WriteF32(number);
                    break;
                case string text:
                {
                    writer.WriteBits(5, 4);
                    var bytes = Encoding.UTF8.GetBytes(text);
                    writer.WriteBits((ulong)bytes.Length, 16);
                    writer.WriteBytes(bytes);
                    break;
                }
                case byte[] bs:
                    writer.WriteBits(6, 4);
                    writer.WriteBits((ulong)bs.Length, 16);
                    writer.WriteBytes(bs);
                    break;
                case Vector3 vector:
                    writer.WriteBits(7, 4);
                    writer.WriteF32(vector.x);
                    writer.WriteF32(vector.y);
                    writer.WriteF32(vector.z);
                    break;
                case Quaternion rotation:
                    writer.WriteBits(8, 4);
                    writer.WriteF32(rotation.x);
                    writer.WriteF32(rotation.y);
                    writer.WriteF32(rotation.z);
                    writer.WriteF32(rotation.w);
                    break;
                default:
                    throw new NetException(
                        "unsupported RPC argument type " + argument?.GetType().Name
                    );
            }
        }

        public static object Read(BitReader reader, Type want)
        {
            switch ((int)reader.ReadBits(4))
            {
                case 1:
                    return reader.ReadBool();
                case 2:
                    return Convert.ChangeType(
                        (int)reader.ReadInt(32),
                        want == typeof(object) ? typeof(int) : want
                    );
                case 3:
                    return reader.ReadInt(64);
                case 4:
                    return Convert.ChangeType(
                        reader.ReadF32(),
                        want == typeof(object) ? typeof(float) : want
                    );
                case 5:
                    return Encoding.UTF8.GetString(reader.ReadBytes((int)reader.ReadBits(16)));
                case 6:
                    return reader.ReadBytes((int)reader.ReadBits(16));
                case 7:
                    return new Vector3(reader.ReadF32(), reader.ReadF32(), reader.ReadF32());
                case 8:
                    return new Quaternion(
                        reader.ReadF32(),
                        reader.ReadF32(),
                        reader.ReadF32(),
                        reader.ReadF32()
                    );
            }
            throw new NetException("bad RPC argument");
        }
    }

    /// <summary>Dev-time grant issuance from the client. Production: your backend calls POST /v1/grants.</summary>
    public static class DevGrant
    {
        public static async Task<string> IssueAsync(
            string http,
            string apiKey,
            string playerId,
            string name
        )
        {
            using (var httpClient = new System.Net.Http.HttpClient())
            {
                var req = new System.Net.Http.HttpRequestMessage(
                    System.Net.Http.HttpMethod.Post,
                    http.TrimEnd('/') + "/v1/grants"
                );
                req.Headers.Authorization = new System.Net.Http.Headers.AuthenticationHeaderValue(
                    "Bearer",
                    apiKey
                );
                req.Content = new System.Net.Http.StringContent(
                    "{\"player_id\":" + Q(playerId) + ",\"name\":" + Q(name) + "}",
                    Encoding.UTF8,
                    "application/json"
                );
                var response = await httpClient.SendAsync(req);
                var body = await response.Content.ReadAsStringAsync();
                if (!response.IsSuccessStatusCode)
                {
                    throw new NetException("grant: " + body);
                }

                return ManagedClientTestHooks.Str(body, "grant");
            }
        }

        static string Q(string text) =>
            "\"" + text.Replace("\\", "\\\\").Replace("\"", "\\\"") + "\"";
    }
}
