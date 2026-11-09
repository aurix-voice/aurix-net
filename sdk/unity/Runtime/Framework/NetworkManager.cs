// Host-authoritative gameplay framework over an Aurix relay room: the server (room master) spawns
// and despawns NetworkIdentities, owns the tick, replicates SyncVar/SyncList/SyncDictionary deltas
// and routes RPCs; clients hold mirrors. Peers are RelayBridge peers (PlayerID = actor id).
// One manager per scene; StartHost() (server + local player), StartServer() (headless) or StartClient().
using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Aurix.Net.Managed;
using Aurix.Net.Transports;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    public interface INetworkObjectFactory
    {
        NetworkIdentity Instantiate(NetworkManager manager, int prefabId);
        void Destroy(NetworkIdentity identity);
    }

    /// <summary>Default factory: Object.Instantiate of <see cref="NetworkManager.networkPrefabs"/>[prefabId].</summary>
    public sealed class PrefabFactory : INetworkObjectFactory
    {
        public NetworkIdentity Instantiate(NetworkManager m, int prefabId)
        {
            if (prefabId < 0 || prefabId >= m.networkPrefabs.Count || m.networkPrefabs[prefabId] == null) throw new NetException("unknown prefab id " + prefabId);
            var go = UnityEngine.Object.Instantiate(m.networkPrefabs[prefabId]);
            var id = go.GetComponent<NetworkIdentity>();
            if (id == null) throw new NetException(m.networkPrefabs[prefabId].name + " has no NetworkIdentity");
            return id;
        }
        public void Destroy(NetworkIdentity id) { if (id != null) UnityEngine.Object.Destroy(id.gameObject); }
    }

    [DefaultExecutionOrder(-100)]
    public sealed class NetworkManager : MonoBehaviour
    {
        enum Msg : byte { Hello = 1, Welcome, PlayerJoined, PlayerLeft, Spawn, Despawn, Owner, Sync, Rpc, ViewTick, ServerTick, StaticRpc, Ping, Pong }
        const byte Version = 1;

        [Header("Aurix Net")]
        public string httpUrl = "http://127.0.0.1:8090";
        public string grant = "";
        public string roomName = "game";
        public int maxPlayers = 16;
        public byte eventCode = 201;
        [Tooltip("Network ticks per second: state deltas are collected and sent at this rate.")]
        public int tickRate = 30;
        [Tooltip("Prefabs that can be spawned; prefabId is the index.")]
        public List<GameObject> networkPrefabs = new List<GameObject>();

        /// <summary>Async grant source; wins over the inspector field.</summary>
        public Func<Task<string>> GrantProvider;
        public enum AutoStart { None, Host, Server, Client, HeadlessServerElseClient }
        [Tooltip("Start a role in Start(): Host, Server (headless), Client, or Server when the build runs without graphics and Client otherwise.")]
        public AutoStart autoStart = AutoStart.None;

        [Tooltip("When the host leaves, the room's new master takes over as server with the world it holds; clients re-attach.")]
        public bool allowHostMigration = true;
        public event Action onPromotedToHost, onHostChanged;
        [Tooltip("StartAuto: room props to match exactly (JSON object), SQL-like lobby expression, and the props a hosted room is created with.")]
        public string matchFilterJson, matchExpr, roomPropsJson;
        [Tooltip("Remote objects are rendered this many ticks behind the newest server state; reported to the server for lag compensation (match NetworkTransform.interpolationTicks).")]
        public float viewDelayTicks = 2f;
        readonly Dictionary<PlayerID, uint> _viewTicks = new Dictionary<PlayerID, uint>();
        /// <summary>Server: the server tick this player is currently looking at (its latest known server tick minus its view delay).</summary>
        public uint ViewTickOf(PlayerID p) => _viewTicks.TryGetValue(p, out var t) ? t : serverTick;

        /// <summary>Server: rewinds every ColliderRollback to the tick <paramref name="shooter"/> was looking at, runs <paramref name="body"/>, restores.</summary>
        public void Rollback(PlayerID shooter, Action body) => Rollback(ViewTickOf(shooter), body);

        public void Rollback(uint tick, Action body)
        {
            if (!isServer) { body(); return; }
            var list = new List<ColliderRollback>();
            foreach (var id in _ids.Values) if (id.isSpawned) foreach (var b in id.behaviours) if (b is ColliderRollback cr) { cr.Rollback(tick); list.Add(cr); }
            try { body(); } finally { foreach (var cr in list) cr.Restore(); }
        }

        public INetworkObjectFactory factory = new PrefabFactory();
        /// <summary>Server: who sees what. Empty = everyone sees everything. All rules must agree (AND).</summary>
        public readonly List<INetworkVisibilityRule> visibilityRules = new List<INetworkVisibilityRule>();
        [Tooltip("Server: re-evaluate visibility rules every N network ticks (0 = only on spawn/join).")]
        public int visibilityIntervalTicks = 5;

        public static NetworkManager main { get; private set; }
        /// <summary>Static RPCs route through <see cref="main"/> (the first manager created, or the one set here).</summary>
        public static void SetMain(NetworkManager m) => main = m;

        public bool isServer { get; private set; }
        public bool isClient { get; private set; }
        public bool isHost => isServer && isClient;
        public bool isOffline => !isServer && !isClient;
        public PlayerID? localPlayer { get; private set; }
        public PlayerID? serverPlayer { get; private set; }
        public uint tick { get; private set; }
        /// <summary>Newest server tick seen (clients); equals <see cref="tick"/> on the server.</summary>
        public uint serverTick { get; private set; }
        /// <summary>Ping/jitter/throughput of this manager's connection (per-player ping on the server).</summary>
        public readonly NetworkStatistics statistics = new NetworkStatistics();
        readonly System.Diagnostics.Stopwatch _clock = System.Diagnostics.Stopwatch.StartNew();
        public BridgeState serverState => _server?.State ?? BridgeState.Stopped;
        public BridgeState clientState => _client?.State ?? BridgeState.Stopped;
        public string lastError => _server?.LastError ?? _client?.LastError;

        readonly List<PlayerID> _players = new List<PlayerID>();
        public IReadOnlyList<PlayerID> players => _players;
        readonly Dictionary<uint, NetworkIdentity> _ids = new Dictionary<uint, NetworkIdentity>();
        public IEnumerable<NetworkIdentity> identities => _ids.Values;
        public bool TryGetIdentity(uint id, out NetworkIdentity ni) => _ids.TryGetValue(id, out ni);

        public event Action<BridgeState> onServerState, onClientState;
        public event Action<PlayerID> onPlayerJoined, onPlayerLeft;
        public event Action<NetworkIdentity> onSpawned, onDespawned;
        public event Action<string> onError;

        readonly Dictionary<uint, NetworkIdentity> _sceneObjects = new Dictionary<uint, NetworkIdentity>();
        /// <summary>Scene-placed identities by sceneId: spawned by the host on start, bound (not instantiated) on clients.</summary>
        public void RegisterSceneObject(NetworkIdentity id) { if (id != null && id.sceneId != 0) _sceneObjects[id.sceneId] = id; }

        RelayBridge _server, _client;
        uint _nextId = 1;
        float _accum;
        readonly List<byte> _toServer = new List<byte>();
        readonly Dictionary<int, List<byte>> _toClient = new Dictionary<int, List<byte>>();
        sealed class Buffered { public uint obj; public byte comp, method; public PlayerID? target; public bool excludeOwner; public byte[] body; }
        readonly List<Buffered> _buffered = new List<Buffered>();
        readonly RpcCodec _codec = new RpcCodec();
        static ulong RpcKey(NetworkIdentity id, NetworkBehaviour b, RpcMethod r) => ((ulong)id.objectId << 16) | ((ulong)b.componentIndex << 8) | r.index;
        static ulong RpcKey(uint obj, byte comp, byte method) => ((ulong)obj << 16) | ((ulong)comp << 8) | method;

        void Awake()
        {
            if (main == null) main = this;
#if UNITY_5_3_OR_NEWER
            foreach (var id in UnityEngine.Object.FindObjectsOfType<NetworkIdentity>(true)) RegisterSceneObject(id);
#endif
        }
        void Start()
        {
            switch (autoStart)
            {
                case AutoStart.Host: StartHost(); break;
                case AutoStart.Server: StartServer(); break;
                case AutoStart.Client: StartClient(); break;
                case AutoStart.HeadlessServerElseClient:
#if UNITY_SERVER
                    StartServer();
#else
                    if (SystemInfo.graphicsDeviceType == UnityEngine.Rendering.GraphicsDeviceType.Null) StartServer(); else StartClient();
#endif
                    break;
            }
        }

        void OnDestroy() { Stop(); if (main == this) main = null; }
        void Update() => NetworkUpdate(Time.unscaledDeltaTime);

        RelayBridge NewBridge() => new RelayBridge(new RelayBridgeSettings
        {
            HttpUrl = httpUrl, Grant = grant, GrantProvider = GrantProvider, RoomName = roomName, MaxPlayers = maxPlayers, EventCode = eventCode, AllowHostMigration = allowHostMigration, MatchFilterJson = matchFilterJson, MatchExpr = matchExpr, PropsJson = roomPropsJson
        });

        // ---- lifecycle ----
        public void StartHost() => StartServerInternal(true);
        public void StartServer() => StartServerInternal(false);

        void StartServerInternal(bool withLocalPlayer)
        {
            if (_server != null || _client != null) return;
            NetSerializer.ResolveIdentity = Resolve;
            var b = _server = NewBridge();
            b.OnState += s => { if (s == BridgeState.Started) ServerStarted(b, withLocalPlayer); onServerState?.Invoke(s); };
            WireServer(b);
            b.OnDisconnected += why => { if (why != BridgeLeave.Local) onError?.Invoke(b.LastError); Teardown(); };
            b.StartHost();
        }

        /// <summary>Quick match: joins a random open room (matchFilterJson / matchExpr) as a client, or hosts a fresh one with roomPropsJson when none exists.</summary>
        public void StartAuto()
        {
            if (_server != null || _client != null) return;
            NetSerializer.ResolveIdentity = Resolve;
            var b = NewBridge(); bool wired = false;
            b.OnState += s =>
            {
                if (s == BridgeState.Started && !wired)
                {
                    wired = true;
                    if (b.IsHost) { _server = b; WireServer(b); ServerStarted(b, true); }
                    else { _client = b; WireClient(b); ClientStarted(b); }
                }
                if (b.IsHost) onServerState?.Invoke(s); else onClientState?.Invoke(s);
            };
            b.OnDisconnected += why => { if (why != BridgeLeave.Local) onError?.Invoke(b.LastError); Teardown(); };
            b.StartAuto();
        }

        void ServerStarted(RelayBridge b, bool withLocalPlayer)
        {
            if (main == null || main == this || (!main.isServer && !main.isClient)) main = this;
            isServer = true; isClient = withLocalPlayer; localPlayer = serverPlayer = new PlayerID((ushort)b.LocalActor); if (withLocalPlayer) _players.Add(localPlayer.Value);
            foreach (var a in b.Peers) { var p = new PlayerID((ushort)a); if (!_players.Contains(p)) _players.Add(p); }
            foreach (var so in _sceneObjects.Values) SpawnExisting(so, -1, null);
        }

        void WireServer(RelayBridge b)
        {
            b.OnPeerJoined += a => { var p = new PlayerID((ushort)a); _players.Add(p); Broadcast(Msg.PlayerJoined, w => w.WriteBits(p.id, 16), except: a); onPlayerJoined?.Invoke(p); };
            b.OnPeerLeft += (a, why) => { var p = new PlayerID((ushort)a); _players.Remove(p); HandlePlayerLeft(p); Broadcast(Msg.PlayerLeft, w => w.WriteBits(p.id, 16)); onPlayerLeft?.Invoke(p); };
            b.OnData += (from, d, _) => HandleFrame(d, new PlayerID((ushort)from), true);
        }

        void ClientStarted(RelayBridge b)
        {
            if (main == null || main == this || (!main.isServer && !main.isClient)) main = this;
            isClient = true; localPlayer = new PlayerID((ushort)b.LocalActor); serverPlayer = new PlayerID((ushort)b.MasterActor);
            Send(-1, Msg.Hello, w => w.WriteBits(Version, 8));   // our data path is up: ask for the world
        }

        public void StartClient()
        {
            if (_server != null || _client != null) return;
            NetSerializer.ResolveIdentity = Resolve;
            var b = _client = NewBridge();
            b.OnState += s => { if (s == BridgeState.Started) ClientStarted(b); onClientState?.Invoke(s); };
            WireClient(b);
            b.OnDisconnected += why => { if (why != BridgeLeave.Local) onError?.Invoke(b.LastError); Teardown(); };
            b.StartClient();
        }

        void WireClient(RelayBridge b)
        {
            b.OnData += (from, d, _) => HandleFrame(d, new PlayerID((ushort)from), isServer);
            b.OnHostChanged += m => { serverPlayer = new PlayerID((ushort)m); Send(-1, Msg.Hello, w => w.WriteBits(Version, 8)); onHostChanged?.Invoke(); };
            b.OnPromoted += () =>
            {
                // we hold the objects we could see; they become the authoritative world
                _server = b; _client = null; isServer = true; serverPlayer = localPlayer;
                b.OnPeerJoined += a => { var p = new PlayerID((ushort)a); if (!_players.Contains(p)) _players.Add(p); Broadcast(Msg.PlayerJoined, w => w.WriteBits(p.id, 16), except: a); onPlayerJoined?.Invoke(p); };
                b.OnPeerLeft += (a, why) => { var p = new PlayerID((ushort)a); _players.Remove(p); HandlePlayerLeft(p); Broadcast(Msg.PlayerLeft, w => w.WriteBits(p.id, 16)); onPlayerLeft?.Invoke(p); };
                _players.Clear(); _players.Add(localPlayer.Value); foreach (var a in b.Peers) _players.Add(new PlayerID((ushort)a));
                uint max = 0; foreach (var id in _ids.Values) { id.ClearObservers(); if (id.objectId > max) max = id.objectId; }
                _nextId = max + 1; serverTick = tick;
                foreach (var id in _ids.Values) foreach (var bh in id.behaviours) foreach (var m in bh.modules) m.ClearDirty();
                onPromotedToHost?.Invoke();
            };
        }

        public void Stop()
        {
            var s = _server; var c = _client; _server = null; _client = null;
            s?.Stop(); c?.Stop();
            Teardown();
        }

        void Teardown()
        {
            foreach (var id in _ids.Values.ToArray()) { id.ClearObservers(); id.MarkDespawned(); onDespawned?.Invoke(id); if (id.sceneId == 0) factory.Destroy(id); }
            _ids.Clear(); _players.Clear(); _buffered.Clear(); _codec.Clear(); _toServer.Clear(); _toClient.Clear();
            isServer = false; isClient = false; localPlayer = null; serverPlayer = null; tick = 0; _nextId = 1;
            _server = null; _client = null;
        }

        NetworkIdentity Resolve(uint id) => _ids.TryGetValue(id, out var ni) ? ni : null;

        // ---- spawning (server) ----
        public NetworkIdentity Spawn(GameObject prefab, PlayerID? owner = null)
        {
            int pid = networkPrefabs.IndexOf(prefab);
            if (pid < 0) throw new NetException(prefab.name + " is not in networkPrefabs");
            return Spawn(pid, owner);
        }

        public NetworkIdentity Spawn(int prefabId, PlayerID? owner = null)
        {
            if (!isServer) throw new NetException("only the server spawns");
            return SpawnExisting(factory.Instantiate(this, prefabId), prefabId, owner);
        }

        /// <summary>Server: put an already instantiated identity (a prefab instance or a scene object) on the network.</summary>
        public NetworkIdentity SpawnExisting(NetworkIdentity id, int prefabId, PlayerID? owner)
        {
            if (!isServer) throw new NetException("only the server spawns");
            if (id.isSpawned) return id;
            id.Setup(this, _nextId++, prefabId, owner);
            _ids[id.objectId] = id;
            foreach (var b in id.behaviours) foreach (var m in b.modules) m.ClearDirty();
            id.MarkSpawned(true);
            foreach (var p in _players) if (p != localPlayer && CanSee(p, id)) { id.AddObserver(p); Send(p.id, Msg.Spawn, w => WriteSpawn(w, id)); }
            onSpawned?.Invoke(id);
            return id;
        }

        public void Despawn(NetworkIdentity id)
        {
            if (!isServer) { Debug.LogWarning("only the server despawns"); return; }
            if (id == null || !_ids.Remove(id.objectId)) return;
            foreach (var p in id.observers) Send(p.id, Msg.Despawn, w => w.WriteBits(id.objectId, 32));
            id.ClearObservers();
            _buffered.RemoveAll(x => x.obj == id.objectId);
            id.MarkDespawned(); onDespawned?.Invoke(id); if (id.sceneId == 0) factory.Destroy(id);
        }

        public void SetOwnership(NetworkIdentity id, PlayerID? player)
        {
            if (!isServer) { Debug.LogWarning("only the server changes ownership"); return; }
            if (id == null || !id.isSpawned || id.owner == player) return;
            if (player.HasValue && !_players.Contains(player.Value)) { Debug.LogWarning($"player {player} is not in the room"); return; }
            id.SetOwner(player);
            if (player.HasValue && player != localPlayer && !id.IsObserver(player.Value)) { id.AddObserver(player.Value); Send(player.Value.id, Msg.Spawn, w => WriteSpawn(w, id)); SendBuffered(player.Value, id); }
            else foreach (var p in id.observers) Send(p.id, Msg.Owner, w => { w.WriteBits(id.objectId, 32); WriteOwner(w, player); });
        }

        void HandlePlayerLeft(PlayerID p)
        {
            _viewTicks.Remove(p);
            _codec.ForgetPeer(p.id); statistics.RemovePlayer(p);
            foreach (var id in _ids.Values) id.RemoveObserver(p);
            foreach (var id in _ids.Values.Where(i => i.owner == p).ToArray())
            {
                if (id.despawnOnOwnerDisconnect) Despawn(id); else SetOwnership(id, null);
            }
        }

        void WriteSpawn(BitWriter w, NetworkIdentity id)
        {
            w.WriteBits(id.objectId, 32); w.WriteBits(id.sceneId, 32); w.WriteUInt((ulong)(id.prefabId < 0 ? 0xffff : id.prefabId), 16); WriteOwner(w, id.owner);
            w.WriteUInt((ulong)id.behaviours.Count, 8);
            foreach (var b in id.behaviours) { w.WriteUInt((ulong)b.modules.Length, 8); foreach (var m in b.modules) m.WriteFull(w); }
        }

        static void WriteOwner(BitWriter w, PlayerID? o) { w.WriteBool(o.HasValue); if (o.HasValue) w.WriteBits(o.Value.id, 16); }
        static PlayerID? ReadOwner(BitReader r) => r.ReadBool() ? new PlayerID((ushort)r.ReadBits(16)) : (PlayerID?)null;

        void SendWelcome(PlayerID p)
        {
            Send(p.id, Msg.Welcome, w =>
            {
                w.WriteBits(Version, 8); w.WriteBits(tick, 32); w.WriteBits(serverPlayer.Value.id, 16);
                w.WriteUInt((ulong)_players.Count, 16); foreach (var x in _players) w.WriteBits(x.id, 16);
            });
            foreach (var id in _ids.Values) if (CanSee(p, id)) { id.AddObserver(p); Send(p.id, Msg.Spawn, w => WriteSpawn(w, id)); SendBuffered(p, id); }
            foreach (var kv in _staticBuffered) { var sr = StaticRpcs.Find(kv.Key); if (sr != null) Send(p.id, Msg.StaticRpc, w => WriteStaticRpc(w, sr, kv.Value)); }
        }

        void SendBuffered(PlayerID p, NetworkIdentity id)
        {
            foreach (var b in _buffered)
            {
                if (b.obj != id.objectId) continue;
                if (b.target.HasValue && b.target.Value != p) continue;
                if (b.excludeOwner && id.owner == p) continue;
                var battr = b.comp < id.behaviours.Count && b.method < id.behaviours[b.comp].rpcs.Length ? id.behaviours[b.comp].rpcs[b.method].attr : null;
                var benc = battr != null ? _codec.Encode(RpcKey(b.obj, b.comp, b.method), p.id, battr, b.body) : b.body;
                Send(p.id, Msg.Rpc, w => { w.WriteBits(b.obj, 32); w.WriteBits(b.comp, 8); w.WriteBits(b.method, 8); w.WriteBits(serverPlayer.Value.id, 16); w.WriteUInt((ulong)benc.Length, 16); w.WriteBytes(benc); });
            }
        }

        /// <summary>Server: may this player hold a copy? Owner and the host's own player always may.</summary>
        public bool CanSee(PlayerID p, NetworkIdentity id)
        {
            if (p == localPlayer || id.owner == p) return true;
            if (id.visibilityRule != null) return id.visibilityRule.CanSee(p, id);
            foreach (var r in visibilityRules) if (!r.CanSee(p, id)) return false;
            return true;
        }

        /// <summary>Server: re-run the rules now; spawns on players that gained sight, despawns on those that lost it.</summary>
        public void EvaluateVisibility()
        {
            if (!isServer) return;
            foreach (var id in _ids.Values.ToArray())
                foreach (var p in _players)
                {
                    if (p == localPlayer) continue;
                    bool see = CanSee(p, id), has = id.IsObserver(p);
                    if (see && !has) { id.AddObserver(p); Send(p.id, Msg.Spawn, w => WriteSpawn(w, id)); SendBuffered(p, id); }
                    else if (!see && has) { id.RemoveObserver(p); Send(p.id, Msg.Despawn, w => w.WriteBits(id.objectId, 32)); }
                }
        }

        // ---- tick ----
        /// <summary>Drives the network: pumps the link, runs ticks and flushes outgoing frames. Called from Update.</summary>
        public void NetworkUpdate(float dt)
        {
            bool connected = (isClient && _client != null && _client.State == BridgeState.Started) || (isServer && _server != null);
            statistics.SetConnected(connected);
            if (statistics.Update(dt) && isClient && !isServer && _client != null && _client.State == BridgeState.Started)
                Send(-1, Msg.Ping, w => { w.WriteBits((uint)_clock.ElapsedMilliseconds, 32); w.WriteBits((ulong)Math.Min(65535, Math.Max(0, statistics.ping)), 16); });
            _server?.Pump(); _client?.Pump();
            if (isOffline) { _server?.Flush(); _client?.Flush(); return; }
            _accum += dt; float step = 1f / Math.Max(1, tickRate);
            int n = 0;
            while (_accum >= step && n++ < 8) { _accum -= step; Tick(); }
            FlushFrames();
            _server?.Flush(); _client?.Flush();
        }

        void Tick()
        {
            tick++; if (isServer) serverTick = tick;
            if (isServer && _players.Count > (isClient ? 1 : 0)) Broadcast(Msg.ServerTick, w => w.WriteBits(tick, 32));   // clients learn the server clock even when nothing else changed
            if (isClient && !isServer && serverTick > 0 && _client != null && _client.State == BridgeState.Started)
            {
                uint vt = serverTick - (uint)Math.Min(serverTick, (uint)Math.Round(viewDelayTicks));
                Send(-1, Msg.ViewTick, w => w.WriteBits(vt, 32));
            }
            if (isServer && visibilityIntervalTicks > 0 && tick % (uint)visibilityIntervalTicks == 0 && (visibilityRules.Count > 0 || _ids.Values.Any(i => i.visibilityRule != null))) EvaluateVisibility();
            float now = tick * (1f / Math.Max(1, tickRate));
            foreach (var id in _ids.Values.ToArray())
            {
                if (!id.isSpawned) continue;
                foreach (var b in id.behaviours)
                {
                    for (int mi = 0; mi < b.modules.Length; mi++)
                    {
                        var m = b.modules[mi];
                        if (m.isController) m.OnBeforeSend();
                        if (!m.isDirty || !m.isController || now < m.nextSendTime) continue;
                        m.nextSendTime = now + m.sendIntervalInSeconds;
                        var body = new BitWriter(); m.WriteDelta(body); m.ClearDirty();
                        var bytes = body.Finish();
                        void Write(BitWriter w) { w.WriteBits(id.objectId, 32); w.WriteBits((ulong)b.componentIndex, 8); w.WriteBits((ulong)mi, 8); w.WriteBits(tick, 32); w.WriteUInt((ulong)bytes.Length, 16); w.WriteBytes(bytes); }
                        if (isServer && m.serverOnly) { m.ClearDirty(); continue; }
                        if (isServer) { if (m.ownerOnly && id.owner.HasValue) { if (id.owner != localPlayer) Send(id.owner.Value.id, Msg.Sync, Write); } else foreach (var p in id.observers) { if (id.lod != null && p != localPlayer && !id.lod.ShouldSendToPlayer(p, tick)) continue; Send(p.id, Msg.Sync, Write); } }
                        else Send(-1, Msg.Sync, Write);
                    }
                    b.OnTick(tick);
                }
            }
        }

        // ---- RPC ----
        /// <summary>Routes an RPC call. Returns true when the call was handled here (sent, buffered or refused)
        /// and the body must NOT run locally; false when the caller should execute the body.</summary>
        internal bool SendRpc(NetworkBehaviour b, RpcMethod r, PlayerID? target, object[] args)
        {
            if (!b.isSpawned) { Debug.LogWarning($"{b.GetType().Name}.{r.method.Name}: object not spawned"); return true; }
            var id = b.identity;
            switch (r.attr.kind)
            {
                case RpcKind.Server:
                {
                    var a = (ServerRpcAttribute)r.attr;
                    if (isServer) return false;
                    if (a.requireOwnership && !id.isOwner) { Debug.LogWarning($"{b.GetType().Name}.{r.method.Name}: ServerRpc requires ownership"); return true; }
                    var bytes = _codec.Encode(RpcKey(id, b, r), -1, a, Pack(b, r, args));
                    Send(-1, Msg.Rpc, w => WriteRpc(w, id, b, r, bytes));
                    return !a.runLocally;
                }
                case RpcKind.Observers:
                {
                    var a = (ObserversRpcAttribute)r.attr;
                    if (!isServer) { Debug.LogWarning($"{b.GetType().Name}.{r.method.Name}: ObserversRpc may only be called on the server"); return true; }
                    var bytes = Pack(b, r, args); var key = RpcKey(id, b, r);
                    foreach (var p in id.observers) { if (p == localPlayer || (a.excludeOwner && id.owner == p)) continue; var enc = _codec.Encode(key, p.id, a, bytes); Send(p.id, Msg.Rpc, w => WriteRpc(w, id, b, r, enc)); }
                    if (a.bufferLast) Buffer(id, b, r, null, a.excludeOwner, bytes);
                    return !(isClient && a.runLocally && !(a.excludeOwner && id.isOwner));
                }
                case RpcKind.Target:
                {
                    var a = (TargetRpcAttribute)r.attr;
                    if (!isServer) { Debug.LogWarning($"{b.GetType().Name}.{r.method.Name}: TargetRpc may only be called on the server"); return true; }
                    if (!target.HasValue) { Debug.LogWarning($"{b.GetType().Name}.{r.method.Name}: TargetRpc needs a target (first parameter PlayerID, or RpcTo)"); return true; }
                    var bytes = Pack(b, r, args);
                    if (a.bufferLast) Buffer(id, b, r, target, false, bytes);
                    if (target == localPlayer) return !(isClient && a.runLocally);
                    if (!id.IsObserver(target.Value)) { Debug.LogWarning($"{b.GetType().Name}.{r.method.Name}: {target} does not see this object"); return true; }
                    { var enc = _codec.Encode(RpcKey(id, b, r), target.Value.id, a, bytes); Send(target.Value.id, Msg.Rpc, w => WriteRpc(w, id, b, r, enc)); };
                    return true;
                }
            }
            return true;
        }

        // ---- static RPCs (woven prologue calls these) ----
        /// <summary>Routes a static RPC through NetworkManager.main. Returns true when the body must NOT run locally.</summary>
        public static bool __StaticRpcSend(uint hash, object[] args)
        {
            var m = main; if (m == null || (!m.isServer && !m.isClient)) return false;
            var r = StaticRpcs.Find(hash); if (r == null) return false;
            return m.SendStaticRpc(r, args);
        }

        public static RpcInfo __StaticLocalInfo() => main == null ? default : new RpcInfo(main.localPlayer ?? default, main.isServer);

        bool SendStaticRpc(StaticRpcMethod r, object[] args)
        {
            string name = r.method.DeclaringType.Name + "." + r.method.Name;
            switch (r.attr.kind)
            {
                case RpcKind.Server:
                {
                    var a = (ServerRpcAttribute)r.attr;
                    if (isServer) return false;
                    var bytes = PackStatic(r, args);
                    Send(-1, Msg.StaticRpc, w => WriteStaticRpc(w, r, bytes));
                    return !a.runLocally;
                }
                case RpcKind.Observers:
                {
                    var a = (ObserversRpcAttribute)r.attr;
                    if (!isServer) { Debug.LogWarning($"{name}: ObserversRpc may only be called on the server"); return true; }
                    var bytes = PackStatic(r, args);
                    foreach (var p in _players) if (p != localPlayer) Send(p.id, Msg.StaticRpc, w => WriteStaticRpc(w, r, bytes));
                    if (a.bufferLast) _staticBuffered[r.hash] = bytes;
                    return !(isClient && a.runLocally);
                }
                case RpcKind.Target:
                {
                    var a = (TargetRpcAttribute)r.attr;
                    if (!isServer) { Debug.LogWarning($"{name}: TargetRpc may only be called on the server"); return true; }
                    if (!(args.Length > 0 && args[0] is PlayerID target)) { Debug.LogWarning($"{name}: TargetRpc needs a PlayerID first parameter"); return true; }
                    if (target == localPlayer) return !(isClient && a.runLocally);
                    if (!_players.Contains(target)) { Debug.LogWarning($"{name}: {target} is not in the room"); return true; }
                    var bytes = PackStatic(r, args);
                    Send(target.id, Msg.StaticRpc, w => WriteStaticRpc(w, r, bytes));
                    return true;
                }
            }
            return true;
        }

        readonly Dictionary<uint, byte[]> _staticBuffered = new Dictionary<uint, byte[]>();
        static byte[] PackStatic(StaticRpcMethod r, object[] args) { var body = new BitWriter(); StaticRpcs.WriteArgs(body, r, args); return body.Finish(); }
        void WriteStaticRpc(BitWriter w, StaticRpcMethod r, byte[] bytes) { w.WriteBits(r.hash, 32); w.WriteBits(localPlayer?.id ?? 0, 16); w.WriteUInt((ulong)bytes.Length, 16); w.WriteBytes(bytes); }

        static byte[] Pack(NetworkBehaviour b, RpcMethod r, object[] args) { var body = new BitWriter(); b.WriteArgs(body, r, args); return body.Finish(); }
        void WriteRpc(BitWriter w, NetworkIdentity id, NetworkBehaviour b, RpcMethod r, byte[] bytes) { w.WriteBits(id.objectId, 32); w.WriteBits((ulong)b.componentIndex, 8); w.WriteBits(r.index, 8); w.WriteBits(localPlayer.Value.id, 16); w.WriteUInt((ulong)bytes.Length, 16); w.WriteBytes(bytes); }

        void Buffer(NetworkIdentity id, NetworkBehaviour b, RpcMethod r, PlayerID? target, bool excludeOwner, byte[] body)
        {
            _buffered.RemoveAll(x => x.obj == id.objectId && x.comp == b.componentIndex && x.method == r.index && x.target.Equals(target));
            _buffered.Add(new Buffered { obj = id.objectId, comp = (byte)b.componentIndex, method = r.index, target = target, excludeOwner = excludeOwner, body = body });
        }

        // ---- framing ----
        void Send(int actor, Msg kind, Action<BitWriter> write)
        {
            var w = new BitWriter(); write(w); var body = w.Finish();
            var buf = actor < 0 ? _toServer : (_toClient.TryGetValue(actor, out var l) ? l : (_toClient[actor] = new List<byte>()));
            buf.Add((byte)kind); buf.Add((byte)(body.Length >> 8)); buf.Add((byte)body.Length); buf.AddRange(body);
            statistics.CountOut(body.Length + 3);
        }

        void Broadcast(Msg kind, Action<BitWriter> write, int except = -1)
        {
            if (_server == null) return;
            foreach (var a in _server.Peers) if (a != except) Send(a, kind, write);
        }

        void FlushFrames()
        {
            if (_server != null)
            {
                foreach (var kv in _toClient) FlushBuffer(kv.Value, bytes => _server.SendTo(kv.Key, bytes, true));
            }
            if (_client != null) FlushBuffer(_toServer, bytes => _client.SendToHost(bytes, true));
        }

        static void FlushBuffer(List<byte> buf, Action<ArraySegment<byte>> send)
        {
            int mtu = RelayBridge.Mtu(true);
            int pos = 0;
            while (pos < buf.Count)
            {
                int end = pos;
                while (end < buf.Count)
                {
                    int len = 3 + ((buf[end + 1] << 8) | buf[end + 2]);
                    if (end + len - pos > mtu && end > pos) break;
                    end += len;
                }
                send(new ArraySegment<byte>(buf.GetRange(pos, end - pos).ToArray()));
                pos = end;
            }
            buf.Clear();
        }

        void HandleFrame(ArraySegment<byte> d, PlayerID from, bool asServer)
        {
            statistics.CountIn(d.Count);
            int pos = d.Offset, end = d.Offset + d.Count; var a = d.Array;
            while (pos + 3 <= end)
            {
                var kind = (Msg)a[pos]; int len = (a[pos + 1] << 8) | a[pos + 2]; pos += 3;
                if (pos + len > end) { Debug.LogWarning("[AurixNet] truncated frame"); return; }
                var body = new byte[len]; Array.Copy(a, pos, body, 0, len); pos += len;
                try { Handle(kind, new BitReader(body), from, asServer); }
                catch (Exception e) { Debug.LogWarning($"[AurixNet] {kind} from {from}: {e.Message}"); }
            }
        }

        void Handle(Msg kind, BitReader r, PlayerID from, bool asServer)
        {
            if (asServer)
            {
                switch (kind)
                {
                    case Msg.StaticRpc:
                    {
                        uint h = (uint)r.ReadBits(32); r.ReadBits(16); var bytes = r.ReadBytes((int)r.ReadUInt(16));
                        var sr = StaticRpcs.Find(h); if (sr == null || sr.attr.kind != RpcKind.Server) return;
                        StaticRpcs.Invoke(sr, new BitReader(bytes), new RpcInfo(from, true));
                        return;
                    }
                    case Msg.Ping: { uint t = (uint)r.ReadBits(32); int reported = (int)r.ReadBits(16); statistics.SetPlayerPing(from, reported); Send(from.id, Msg.Pong, w => w.WriteBits(t, 32)); return; }
                    case Msg.ViewTick: { uint vt = (uint)r.ReadBits(32); if (vt <= serverTick) _viewTicks[from] = vt; return; }
                    case Msg.Hello: if (r.ReadBits(8) == Version && _players.Contains(from)) SendWelcome(from); else Debug.LogWarning($"[AurixNet] hello from {from} with another protocol version"); return;
                    case Msg.Sync: { var (id, b, m) = ReadModuleRef(r); r.ReadBits(32); var bytes = r.ReadBytes((int)r.ReadUInt(16)); if (id == null || !m.ownerAuth || id.owner != from) return; m.lastReceivedTick = tick; bool accepted = m.ReadDeltaFromOwner(new BitReader(bytes)); m.ClearDirty();
                        if (!accepted) { var cw = new BitWriter(); m.WriteDelta(cw); var cb = cw.Finish(); Send(from.id, Msg.Sync, w => { w.WriteBits(id.objectId, 32); w.WriteBits((ulong)b.componentIndex, 8); w.WriteBits((ulong)m.index, 8); w.WriteBits(tick, 32); w.WriteUInt((ulong)cb.Length, 16); w.WriteBytes(cb); }); return; }
                        void Write(BitWriter w) { w.WriteBits(id.objectId, 32); w.WriteBits((ulong)b.componentIndex, 8); w.WriteBits((ulong)m.index, 8); w.WriteBits(tick, 32); w.WriteUInt((ulong)bytes.Length, 16); w.WriteBytes(bytes); }
                        if (!m.ownerOnly && !m.serverOnly) foreach (var p in id.observers) if (p != from) Send(p.id, Msg.Sync, Write); return; }
                    case Msg.Rpc:
                    {
                        var (id, b, _) = ReadModuleRef(r, module: false); byte mi = (byte)r.ReadBits(8); r.ReadBits(16); var bytes = r.ReadBytes((int)r.ReadUInt(16));
                        if (id == null || mi >= b.rpcs.Length) return;
                        var rm = b.rpcs[mi]; if (rm.attr.kind != RpcKind.Server) return;
                        bytes = _codec.Decode(RpcKey(id, b, rm), from.id, rm.attr, bytes);
                        if (((ServerRpcAttribute)rm.attr).requireOwnership && id.owner != from) { Debug.LogWarning($"[AurixNet] {b.GetType().Name}.{rm.method.Name} from non-owner {from}"); return; }
                        b.InvokeRpc(rm, new BitReader(bytes), new RpcInfo(from, true));
                        return;
                    }
                }
                return;
            }
            if (from != serverPlayer) return;
            switch (kind)
            {
                case Msg.Welcome:
                {
                    if (r.ReadBits(8) != Version) { onError?.Invoke("protocol version mismatch"); Stop(); return; }
                    tick = serverTick = (uint)r.ReadBits(32); r.ReadBits(16);
                    _players.Clear(); int n = (int)r.ReadUInt(16); for (int i = 0; i < n; i++) _players.Add(new PlayerID((ushort)r.ReadBits(16)));
                    return;
                }
                case Msg.StaticRpc:
                {
                    uint h = (uint)r.ReadBits(32); var sender = new PlayerID((ushort)r.ReadBits(16)); var bytes = r.ReadBytes((int)r.ReadUInt(16));
                    var sr = StaticRpcs.Find(h); if (sr == null || sr.attr.kind == RpcKind.Server) return;
                    StaticRpcs.Invoke(sr, new BitReader(bytes), new RpcInfo(sender, false));
                    return;
                }
                case Msg.Pong: { uint t = (uint)r.ReadBits(32); statistics.RecordRtt((uint)_clock.ElapsedMilliseconds - t); return; }
                case Msg.ServerTick: { uint st = (uint)r.ReadBits(32); if (st > serverTick) serverTick = st; return; }
                case Msg.PlayerJoined: { var p = new PlayerID((ushort)r.ReadBits(16)); if (!_players.Contains(p)) _players.Add(p); onPlayerJoined?.Invoke(p); return; }
                case Msg.PlayerLeft: { var p = new PlayerID((ushort)r.ReadBits(16)); _players.Remove(p); onPlayerLeft?.Invoke(p); return; }
                case Msg.Spawn:
                {
                    uint oid = (uint)r.ReadBits(32); uint sid = (uint)r.ReadBits(32); int pid = (int)r.ReadUInt(16); if (pid == 0xffff) pid = -1; var owner = ReadOwner(r);
                    if (_ids.ContainsKey(oid)) return;
                    NetworkIdentity id;
                    if (sid != 0) { if (!_sceneObjects.TryGetValue(sid, out id)) { Debug.LogWarning($"[AurixNet] unknown scene object {sid}"); return; } }
                    else id = factory.Instantiate(this, pid);
                    id.Setup(this, oid, pid, owner); _ids[oid] = id;
                    int nb = (int)r.ReadUInt(8);
                    for (int i = 0; i < nb; i++) { int nm = (int)r.ReadUInt(8); for (int j = 0; j < nm; j++) if (i < id.behaviours.Count && j < id.behaviours[i].modules.Length) { var mod = id.behaviours[i].modules[j]; mod.lastReceivedTick = serverTick; mod.ReadFull(r); } }
                    foreach (var b in id.behaviours) foreach (var m in b.modules) m.ClearDirty();
                    id.MarkSpawned(false); onSpawned?.Invoke(id);
                    return;
                }
                case Msg.Despawn:
                {
                    uint oid = (uint)r.ReadBits(32);
                    if (_ids.TryGetValue(oid, out var id)) { _ids.Remove(oid); id.MarkDespawned(); onDespawned?.Invoke(id); if (id.sceneId == 0) factory.Destroy(id); }
                    return;
                }
                case Msg.Owner: { uint oid = (uint)r.ReadBits(32); var o = ReadOwner(r); if (_ids.TryGetValue(oid, out var id)) id.SetOwner(o); return; }
                case Msg.Sync: { var (id, _, m) = ReadModuleRef(r); uint st = (uint)r.ReadBits(32); var bytes = r.ReadBytes((int)r.ReadUInt(16)); if (st > serverTick) serverTick = st; if (id != null) { m.lastReceivedTick = st; m.ReadDelta(new BitReader(bytes)); m.ClearDirty(); } return; }
                case Msg.Rpc:
                {
                    var (id, b, _) = ReadModuleRef(r, module: false); byte mi = (byte)r.ReadBits(8); var sender = new PlayerID((ushort)r.ReadBits(16)); var bytes = r.ReadBytes((int)r.ReadUInt(16));
                    if (id == null || mi >= b.rpcs.Length) return;
                    var rm = b.rpcs[mi]; if (rm.attr.kind == RpcKind.Server) return;
                    bytes = _codec.Decode(RpcKey(id, b, rm), -1, rm.attr, bytes);
                    b.InvokeRpc(rm, new BitReader(bytes), new RpcInfo(sender, false));
                    return;
                }
            }
        }

        (NetworkIdentity, NetworkBehaviour, NetworkModule) ReadModuleRef(BitReader r, bool module = true)
        {
            uint oid = (uint)r.ReadBits(32); int bi = (int)r.ReadBits(8); int mi = module ? (int)r.ReadBits(8) : -1;
            if (!_ids.TryGetValue(oid, out var id) || bi >= id.behaviours.Count) return (null, null, null);
            var b = id.behaviours[bi];
            if (module && mi >= b.modules.Length) return (null, null, null);
            return (id, b, module ? b.modules[mi] : null);
        }
    }
}
