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

    /// <summary>
    /// Factory whose prefabs may need loading first (Addressables, asset bundles). The manager keeps the spawn and every
    /// message for that object pending until <c>ready</c> is called, then applies them in order.
    /// </summary>
    public interface IAsyncNetworkObjectFactory : INetworkObjectFactory
    {
        /// <summary>Instantiate when the prefab is available; call <paramref name="ready"/> on the main thread (may be synchronous).</summary>
        void InstantiateAsync(NetworkManager manager, int prefabId, Action<NetworkIdentity> ready);
    }

    /// <summary>Default factory: Object.Instantiate of <see cref="NetworkManager.networkPrefabs"/>[prefabId].</summary>
    public sealed class PrefabFactory : INetworkObjectFactory
    {
        public NetworkIdentity Instantiate(NetworkManager manager, int prefabId)
        {
            if (
                prefabId < 0
                || prefabId >= manager.networkPrefabs.Count
                || manager.networkPrefabs[prefabId] == null
            )
            {
                throw new NetException("unknown prefab id " + prefabId);
            }

            var go = UnityEngine.Object.Instantiate(manager.networkPrefabs[prefabId]);
            var id = go.GetComponent<NetworkIdentity>();
            if (id == null)
            {
                throw new NetException(
                    manager.networkPrefabs[prefabId].name + " has no NetworkIdentity"
                );
            }

            return id;
        }

        public void Destroy(NetworkIdentity id)
        {
            if (id != null)
            {
                UnityEngine.Object.Destroy(id.gameObject);
            }
        }
    }

    [DefaultExecutionOrder(-100)]
    public sealed class NetworkManager : MonoBehaviour
    {
        enum Msg : byte
        {
            Hello = 1,
            Welcome,
            PlayerJoined,
            PlayerLeft,
            Spawn,
            Despawn,
            Owner,
            Sync,
            Rpc,
            ViewTick,
            ServerTick,
            StaticRpc,
            Ping,
            Pong,
            SceneLoad,
            SceneReady,
        }

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

        public enum AutoStart
        {
            None,
            Host,
            Server,
            Client,
            HeadlessServerElseClient,
        }

        [Tooltip(
            "Start a role in Start(): Host, Server (headless), Client, or Server when the build runs without graphics and Client otherwise."
        )]
        public AutoStart autoStart = AutoStart.None;

        [Tooltip(
            "When the host leaves, the room's new master takes over as server with the world it holds; clients re-attach."
        )]
        public bool allowHostMigration = true;
        public event Action onPromotedToHost,
            onHostChanged;

        [Tooltip(
            "StartAuto: room props to match exactly (JSON object), SQL-like lobby expression, and the props a hosted room is created with."
        )]
        public string matchFilterJson,
            matchExpr,
            roomPropsJson;

        [Tooltip(
            "Remote objects are rendered this many ticks behind the newest server state; reported to the server for lag compensation (match NetworkTransform.interpolationTicks)."
        )]
        public float viewDelayTicks = 2f;
        readonly Dictionary<PlayerID, uint> _viewTicks = new Dictionary<PlayerID, uint>();

        /// <summary>Server: the server tick this player is currently looking at (its latest known server tick minus its view delay).</summary>
        public uint ViewTickOf(PlayerID player) =>
            _viewTicks.TryGetValue(player, out var tick) ? tick : serverTick;

        /// <summary>Server: rewinds every ColliderRollback to the tick <paramref name="shooter"/> was looking at, runs <paramref name="body"/>, restores.</summary>
        public void Rollback(PlayerID shooter, Action body) => Rollback(ViewTickOf(shooter), body);

        public void Rollback(uint tick, Action body)
        {
            if (!isServer)
            {
                body();
                return;
            }
            var list = new List<ColliderRollback>();
            foreach (var id in _ids.Values)
            {
                if (id.isSpawned)
                {
                    foreach (var behaviour in id.behaviours)
                    {
                        if (behaviour is ColliderRollback cr)
                        {
                            cr.Rollback(tick);
                            list.Add(cr);
                        }
                    }
                }
            }

            try
            {
                body();
            }
            finally
            {
                foreach (var cr in list)
                {
                    cr.Restore();
                }
            }
        }

        public INetworkObjectFactory factory = new PrefabFactory();

        /// <summary>Server: who sees what. Empty = everyone sees everything. All rules must agree (AND).</summary>
        public readonly List<INetworkVisibilityRule> visibilityRules =
            new List<INetworkVisibilityRule>();

        [Tooltip(
            "Server: re-evaluate visibility rules every N network ticks (0 = only on spawn/join)."
        )]
        public int visibilityIntervalTicks = 5;

        public static NetworkManager main { get; private set; }

        /// <summary>Static RPCs route through <see cref="main"/> (the first manager created, or the one set here).</summary>
        public static void SetMain(NetworkManager manager) => main = manager;

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

        public event Action<BridgeState> onServerState,
            onClientState;
        public event Action<PlayerID> onPlayerJoined,
            onPlayerLeft;
        public event Action<NetworkIdentity> onSpawned,
            onDespawned;
        public event Action<string> onError;

        /// <summary>Synchronised scene load (server LoadScene): start on every peer, done when all players reported ready (or sceneLoadTimeout passed).</summary>
        public event Action<string> onSceneLoadStart,
            onSceneLoadDone;

        /// <summary>How the scene is actually loaded; defaults to SceneManager.LoadSceneAsync in Unity, a no-op elsewhere. Replace for Addressables/custom loaders.</summary>
        public Func<string, Task> sceneLoader = DefaultSceneLoader;

        /// <summary>Seconds the server waits for every player's ready before finishing the load anyway.</summary>
        public float sceneLoadTimeout = 30f;

        /// <summary>Name of the scene a LoadScene is in flight for, null otherwise.</summary>
        public string loadingScene { get; private set; }
        public bool isSceneLoading => loadingScene != null;
        ushort _sceneLoadId;
        readonly HashSet<PlayerID> _sceneReady = new HashSet<PlayerID>();
        HashSet<PlayerID> _sceneWaitFor;
        float _sceneDeadline;

        /// <summary>Server only: load a scene on every peer in lock step. Clients pause their view until the server says done; late joiners get the current scene name in Welcome.</summary>
        public void LoadScene(string sceneName)
        {
            if (!isServer || string.IsNullOrEmpty(sceneName))
            {
                return;
            }

            _sceneLoadId++;
            loadingScene = sceneName;
            _sceneReady.Clear();
            _sceneWaitFor = new HashSet<PlayerID>(_players);
            _sceneDeadline = (float)_clock.Elapsed.TotalSeconds + sceneLoadTimeout;
            Broadcast(
                Msg.SceneLoad,
                writer =>
                {
                    writer.WriteBits(_sceneLoadId, 16);
                    WriteString(writer, sceneName);
                }
            );
            onSceneLoadStart?.Invoke(sceneName);
            var id = _sceneLoadId;
            RunSceneLoad(
                sceneName,
                () =>
                {
                    if (id == _sceneLoadId && localPlayer.HasValue)
                    {
                        SceneReadyFrom(localPlayer.Value);
                    }
                }
            );
        }

        void SceneReadyFrom(PlayerID player)
        {
            if (!isServer || loadingScene == null)
            {
                return;
            }

            _sceneReady.Add(player);
            if (
                _sceneWaitFor != null
                && _sceneWaitFor.All(x => _sceneReady.Contains(x) || !_players.Contains(x))
            )
            {
                FinishSceneLoad();
            }
        }

        void FinishSceneLoad()
        {
            var name = loadingScene;
            loadingScene = null;
            _sceneWaitFor = null;
            if (isServer)
            {
                Broadcast(Msg.SceneReady, writer => writer.WriteBits(_sceneLoadId, 16));
            }

            onSceneLoadDone?.Invoke(name);
        }

        void TickSceneLoad()
        {
            if (
                isServer
                && loadingScene != null
                && (float)_clock.Elapsed.TotalSeconds >= _sceneDeadline
            )
            {
                FinishSceneLoad();
            }
        }

        void RunSceneLoad(string sceneName, Action done)
        {
            var task = sceneLoader != null ? sceneLoader(sceneName) : Task.CompletedTask;
            if (task.IsCompleted)
            {
                done();
                return;
            }
            _pendingSceneLoad = (task, done);
        }

        (Task task, Action done)? _pendingSceneLoad;

        void PollSceneLoad()
        {
            if (_pendingSceneLoad is { } pending && pending.task.IsCompleted)
            {
                _pendingSceneLoad = null;
                pending.done();
            }
        }

        static void WriteString(BitWriter writer, string text)
        {
            var bytes = System.Text.Encoding.UTF8.GetBytes(text ?? "");
            writer.WriteBits((uint)Math.Min(bytes.Length, 255), 8);
            for (int i = 0; i < Math.Min(bytes.Length, 255); i++)
            {
                writer.WriteBits(bytes[i], 8);
            }
        }

        static string ReadString(BitReader reader)
        {
            int count = (int)reader.ReadBits(8);
            var bytes = new byte[count];
            for (int i = 0; i < count; i++)
            {
                bytes[i] = (byte)reader.ReadBits(8);
            }

            return System.Text.Encoding.UTF8.GetString(bytes);
        }

        static Task DefaultSceneLoader(string sceneName)
        {
#if UNITY_5_3_OR_NEWER
            var tcs = new TaskCompletionSource<bool>();
            var op = UnityEngine.SceneManagement.SceneManager.LoadSceneAsync(sceneName);
            if (op == null)
            {
                tcs.SetResult(false);
            }
            else
            {
                op.completed += _ => tcs.SetResult(true);
            }
            return tcs.Task;
#else
            return Task.CompletedTask;
#endif
        }

        readonly Dictionary<uint, NetworkIdentity> _sceneObjects =
            new Dictionary<uint, NetworkIdentity>();

        /// <summary>Scene-placed identities by sceneId: spawned by the host on start, bound (not instantiated) on clients.</summary>
        public void RegisterSceneObject(NetworkIdentity id)
        {
            if (id != null && id.sceneId != 0)
            {
                _sceneObjects[id.sceneId] = id;
            }
        }

        RelayBridge _server,
            _client;
        uint _nextId = 1;
        float _accum;
        readonly List<byte> _toServer = new List<byte>();
        readonly Dictionary<int, List<byte>> _toClient = new Dictionary<int, List<byte>>();

        sealed class Buffered
        {
            public uint obj;
            public byte comp,
                method;
            public PlayerID? target;
            public bool excludeOwner;
            public byte[] body;
        }

        readonly List<Buffered> _buffered = new List<Buffered>();
        readonly RpcCodec _codec = new RpcCodec();
        readonly Dictionary<uint, List<(Msg kind, byte[] body)>> _pendingSpawns =
            new Dictionary<uint, List<(Msg, byte[])>>();
        public int pendingSpawnCount => _pendingSpawns.Count;

        static ulong RpcKey(NetworkIdentity id, NetworkBehaviour behaviour, RpcMethod method) =>
            ((ulong)id.objectId << 16) | ((ulong)behaviour.componentIndex << 8) | method.index;

        static ulong RpcKey(uint obj, byte comp, byte method) =>
            ((ulong)obj << 16) | ((ulong)comp << 8) | method;

        void Awake()
        {
            if (main == null)
            {
                main = this;
            }
#if UNITY_5_3_OR_NEWER
            foreach (var id in UnityEngine.Object.FindObjectsOfType<NetworkIdentity>(true))
            {
                RegisterSceneObject(id);
            }
#endif
        }

        void Start()
        {
            switch (autoStart)
            {
                case AutoStart.Host:
                    StartHost();
                    break;
                case AutoStart.Server:
                    StartServer();
                    break;
                case AutoStart.Client:
                    StartClient();
                    break;
                case AutoStart.HeadlessServerElseClient:
#if UNITY_SERVER
                    StartServer();
#else
                    if (
                        SystemInfo.graphicsDeviceType
                        == UnityEngine.Rendering.GraphicsDeviceType.Null
                    )
                    {
                        StartServer();
                    }
                    else
                    {
                        StartClient();
                    }
#endif
                    break;
            }
        }

        void OnDestroy()
        {
            Stop();
            if (main == this)
            {
                main = null;
            }
        }

        void Update() => NetworkUpdate(Time.unscaledDeltaTime);

        RelayBridge NewBridge() =>
            new RelayBridge(
                new RelayBridgeSettings
                {
                    HttpUrl = httpUrl,
                    Grant = grant,
                    GrantProvider = GrantProvider,
                    RoomName = roomName,
                    MaxPlayers = maxPlayers,
                    EventCode = eventCode,
                    AllowHostMigration = allowHostMigration,
                    MatchFilterJson = matchFilterJson,
                    MatchExpr = matchExpr,
                    PropsJson = roomPropsJson,
                }
            );

        // ---- lifecycle ----
        public void StartHost() => StartServerInternal(true);

        public void StartServer() => StartServerInternal(false);

        /// <summary>Dedicated-server topology: the headless server creates its room through the node running on the same machine
        /// (default http://127.0.0.1:8090); players keep the project URL and are redirected straight to this machine.</summary>
        public void StartDedicated(string localNodeUrl = "http://127.0.0.1:8090")
        {
            if (!string.IsNullOrEmpty(localNodeUrl))
            {
                httpUrl = localNodeUrl;
            }

            StartServerInternal(false);
        }

        void StartServerInternal(bool withLocalPlayer)
        {
            if (_server != null || _client != null)
            {
                return;
            }

            NetSerializer.ResolveIdentity = Resolve;
            var bridge = _server = NewBridge();
            bridge.OnState += state =>
            {
                if (state == BridgeState.Started)
                {
                    ServerStarted(bridge, withLocalPlayer);
                }

                onServerState?.Invoke(state);
            };
            WireServer(bridge);
            bridge.OnDisconnected += why =>
            {
                if (why != BridgeLeave.Local)
                {
                    onError?.Invoke(bridge.LastError);
                }

                Teardown();
            };
            bridge.StartHost();
        }

        /// <summary>Quick match: joins a random open room (matchFilterJson / matchExpr) as a client, or hosts a fresh one with roomPropsJson when none exists.</summary>
        public void StartAuto()
        {
            if (_server != null || _client != null)
            {
                return;
            }

            NetSerializer.ResolveIdentity = Resolve;
            var bridge = NewBridge();
            bool wired = false;
            bridge.OnState += state =>
            {
                if (state == BridgeState.Started && !wired)
                {
                    wired = true;
                    if (bridge.IsHost)
                    {
                        _server = bridge;
                        WireServer(bridge);
                        ServerStarted(bridge, true);
                    }
                    else
                    {
                        _client = bridge;
                        WireClient(bridge);
                        ClientStarted(bridge);
                    }
                }
                if (bridge.IsHost)
                {
                    onServerState?.Invoke(state);
                }
                else
                {
                    onClientState?.Invoke(state);
                }
            };
            bridge.OnDisconnected += why =>
            {
                if (why != BridgeLeave.Local)
                {
                    onError?.Invoke(bridge.LastError);
                }

                Teardown();
            };
            bridge.StartAuto();
        }

        void ServerStarted(RelayBridge bridge, bool withLocalPlayer)
        {
            if (main == null || main == this || (!main.isServer && !main.isClient))
            {
                main = this;
            }

            isServer = true;
            isClient = withLocalPlayer;
            localPlayer = serverPlayer = new PlayerID((ushort)bridge.LocalActor);
            if (withLocalPlayer)
            {
                _players.Add(localPlayer.Value);
            }

            foreach (var actor in bridge.Peers)
            {
                var player = new PlayerID((ushort)actor);
                if (!_players.Contains(player))
                {
                    _players.Add(player);
                }
            }
            foreach (var so in _sceneObjects.Values)
            {
                SpawnExisting(so, -1, null);
            }
        }

        void WireServer(RelayBridge bridge)
        {
            bridge.OnPeerJoined += actor =>
            {
                var player = new PlayerID((ushort)actor);
                _players.Add(player);
                Broadcast(
                    Msg.PlayerJoined,
                    writer => writer.WriteBits(player.id, 16),
                    except: actor
                );
                onPlayerJoined?.Invoke(player);
            };
            bridge.OnPeerLeft += (actor, why) =>
            {
                var player = new PlayerID((ushort)actor);
                _players.Remove(player);
                HandlePlayerLeft(player);
                Broadcast(Msg.PlayerLeft, writer => writer.WriteBits(player.id, 16));
                onPlayerLeft?.Invoke(player);
            };
            bridge.OnData += (from, data, _) => HandleFrame(data, new PlayerID((ushort)from), true);
        }

        void ClientStarted(RelayBridge bridge)
        {
            if (main == null || main == this || (!main.isServer && !main.isClient))
            {
                main = this;
            }

            isClient = true;
            localPlayer = new PlayerID((ushort)bridge.LocalActor);
            serverPlayer = new PlayerID((ushort)bridge.MasterActor);
            Send(-1, Msg.Hello, writer => writer.WriteBits(Version, 8)); // our data path is up: ask for the world
        }

        public void StartClient()
        {
            if (_server != null || _client != null)
            {
                return;
            }

            NetSerializer.ResolveIdentity = Resolve;
            var bridge = _client = NewBridge();
            bridge.OnState += state =>
            {
                if (state == BridgeState.Started)
                {
                    ClientStarted(bridge);
                }

                onClientState?.Invoke(state);
            };
            WireClient(bridge);
            bridge.OnDisconnected += why =>
            {
                if (why != BridgeLeave.Local)
                {
                    onError?.Invoke(bridge.LastError);
                }

                Teardown();
            };
            bridge.StartClient();
        }

        void WireClient(RelayBridge bridge)
        {
            bridge.OnData += (from, data, _) =>
                HandleFrame(data, new PlayerID((ushort)from), isServer);
            bridge.OnHostChanged += master =>
            {
                serverPlayer = new PlayerID((ushort)master);
                Send(-1, Msg.Hello, writer => writer.WriteBits(Version, 8));
                onHostChanged?.Invoke();
            };
            bridge.OnPromoted += () =>
            {
                // we hold the objects we could see; they become the authoritative world
                _server = bridge;
                _client = null;
                isServer = true;
                serverPlayer = localPlayer;
                bridge.OnPeerJoined += actor =>
                {
                    var player = new PlayerID((ushort)actor);
                    if (!_players.Contains(player))
                    {
                        _players.Add(player);
                    }

                    Broadcast(
                        Msg.PlayerJoined,
                        writer => writer.WriteBits(player.id, 16),
                        except: actor
                    );
                    onPlayerJoined?.Invoke(player);
                };
                bridge.OnPeerLeft += (actor, why) =>
                {
                    var player = new PlayerID((ushort)actor);
                    _players.Remove(player);
                    HandlePlayerLeft(player);
                    Broadcast(Msg.PlayerLeft, writer => writer.WriteBits(player.id, 16));
                    onPlayerLeft?.Invoke(player);
                };
                _players.Clear();
                _players.Add(localPlayer.Value);
                foreach (var actor in bridge.Peers)
                {
                    _players.Add(new PlayerID((ushort)actor));
                }

                uint max = 0;
                foreach (var id in _ids.Values)
                {
                    id.ClearObservers();
                    if (id.objectId > max)
                    {
                        max = id.objectId;
                    }
                }
                _nextId = max + 1;
                serverTick = tick;
                foreach (var id in _ids.Values)
                {
                    foreach (var bh in id.behaviours)
                    {
                        foreach (var module in bh.modules)
                        {
                            module.ClearDirty();
                        }
                    }
                }

                onPromotedToHost?.Invoke();
            };
        }

        public void Stop()
        {
            var server = _server;
            var client = _client;
            _server = null;
            _client = null;
            server?.Stop();
            client?.Stop();
            Teardown();
        }

        void Teardown()
        {
            foreach (var id in _ids.Values.ToArray())
            {
                id.ClearObservers();
                id.MarkDespawned();
                onDespawned?.Invoke(id);
                if (id.sceneId == 0)
                {
                    factory.Destroy(id);
                }
            }
            _ids.Clear();
            _players.Clear();
            _buffered.Clear();
            _codec.Clear();
            _pendingSpawns.Clear();
            _toServer.Clear();
            _toClient.Clear();
            isServer = false;
            isClient = false;
            localPlayer = null;
            serverPlayer = null;
            tick = 0;
            _nextId = 1;
            _server = null;
            _client = null;
        }

        NetworkIdentity Resolve(uint id) => _ids.TryGetValue(id, out var ni) ? ni : null;

        // ---- spawning (server) ----
        public NetworkIdentity Spawn(GameObject prefab, PlayerID? owner = null)
        {
            int pid = networkPrefabs.IndexOf(prefab);
            if (pid < 0)
            {
                throw new NetException(prefab.name + " is not in networkPrefabs");
            }

            return Spawn(pid, owner);
        }

        public NetworkIdentity Spawn(int prefabId, PlayerID? owner = null)
        {
            if (!isServer)
            {
                throw new NetException("only the server spawns");
            }

            return SpawnExisting(factory.Instantiate(this, prefabId), prefabId, owner);
        }

        /// <summary>Server: put an already instantiated identity (a prefab instance or a scene object) on the network.</summary>
        public NetworkIdentity SpawnExisting(NetworkIdentity id, int prefabId, PlayerID? owner)
        {
            if (!isServer)
            {
                throw new NetException("only the server spawns");
            }

            if (id.isSpawned)
            {
                return id;
            }

            id.Setup(this, _nextId++, prefabId, owner);
            _ids[id.objectId] = id;
            foreach (var behaviour in id.behaviours)
            {
                foreach (var module in behaviour.modules)
                {
                    module.ClearDirty();
                }
            }

            id.MarkSpawned(true);
            foreach (var player in _players)
            {
                if (player != localPlayer && CanSee(player, id))
                {
                    id.AddObserver(player);
                    Send(player.id, Msg.Spawn, writer => WriteSpawn(writer, id));
                }
            }

            onSpawned?.Invoke(id);
            return id;
        }

        public void Despawn(NetworkIdentity id)
        {
            if (!isServer)
            {
                Debug.LogWarning("only the server despawns");
                return;
            }
            if (id == null || !_ids.Remove(id.objectId))
            {
                return;
            }

            foreach (var player in id.observers)
            {
                Send(player.id, Msg.Despawn, writer => writer.WriteBits(id.objectId, 32));
            }

            id.ClearObservers();
            _buffered.RemoveAll(x => x.obj == id.objectId);
            id.MarkDespawned();
            onDespawned?.Invoke(id);
            if (id.sceneId == 0)
            {
                factory.Destroy(id);
            }
        }

        public void SetOwnership(NetworkIdentity id, PlayerID? player)
        {
            if (!isServer)
            {
                Debug.LogWarning("only the server changes ownership");
                return;
            }
            if (id == null || !id.isSpawned || id.owner == player)
            {
                return;
            }

            if (player.HasValue && !_players.Contains(player.Value))
            {
                Debug.LogWarning($"player {player} is not in the room");
                return;
            }
            id.SetOwner(player);
            if (player.HasValue && player != localPlayer && !id.IsObserver(player.Value))
            {
                id.AddObserver(player.Value);
                Send(player.Value.id, Msg.Spawn, writer => WriteSpawn(writer, id));
                SendBuffered(player.Value, id);
            }
            else
            {
                foreach (var observer in id.observers)
                {
                    Send(
                        observer.id,
                        Msg.Owner,
                        writer =>
                        {
                            writer.WriteBits(id.objectId, 32);
                            WriteOwner(writer, player);
                        }
                    );
                }
            }
        }

        void HandlePlayerLeft(PlayerID player)
        {
            _viewTicks.Remove(player);
            _codec.ForgetPeer(player.id);
            statistics.RemovePlayer(player);
            foreach (var id in _ids.Values)
            {
                id.RemoveObserver(player);
            }

            foreach (var id in _ids.Values.Where(identity => identity.owner == player).ToArray())
            {
                if (id.despawnOnOwnerDisconnect)
                {
                    Despawn(id);
                }
                else
                {
                    SetOwnership(id, null);
                }
            }
        }

        void WriteSpawn(BitWriter writer, NetworkIdentity id)
        {
            writer.WriteBits(id.objectId, 32);
            writer.WriteBits(id.sceneId, 32);
            writer.WriteUInt((ulong)(id.prefabId < 0 ? 0xffff : id.prefabId), 16);
            WriteOwner(writer, id.owner);
            writer.WriteUInt((ulong)id.behaviours.Count, 8);
            foreach (var behaviour in id.behaviours)
            {
                writer.WriteUInt((ulong)behaviour.modules.Length, 8);
                foreach (var module in behaviour.modules)
                {
                    module.WriteFull(writer);
                }
            }
        }

        static void WriteOwner(BitWriter writer, PlayerID? owner)
        {
            writer.WriteBool(owner.HasValue);
            if (owner.HasValue)
            {
                writer.WriteBits(owner.Value.id, 16);
            }
        }

        static PlayerID? ReadOwner(BitReader reader) =>
            reader.ReadBool() ? new PlayerID((ushort)reader.ReadBits(16)) : (PlayerID?)null;

        void SendWelcome(PlayerID player)
        {
            Send(
                player.id,
                Msg.Welcome,
                writer =>
                {
                    writer.WriteBits(Version, 8);
                    writer.WriteBits(tick, 32);
                    writer.WriteBits(serverPlayer.Value.id, 16);
                    writer.WriteUInt((ulong)_players.Count, 16);
                    foreach (var player in _players)
                    {
                        writer.WriteBits(player.id, 16);
                    }
                }
            );
            foreach (var id in _ids.Values)
            {
                if (CanSee(player, id))
                {
                    id.AddObserver(player);
                    Send(player.id, Msg.Spawn, writer => WriteSpawn(writer, id));
                    SendBuffered(player, id);
                }
            }

            foreach (var kv in _staticBuffered)
            {
                var sr = StaticRpcs.Find(kv.Key);
                if (sr != null)
                {
                    Send(player.id, Msg.StaticRpc, writer => WriteStaticRpc(writer, sr, kv.Value));
                }
            }
        }

        void SendBuffered(PlayerID player, NetworkIdentity id)
        {
            foreach (var buffered in _buffered)
            {
                if (buffered.obj != id.objectId)
                {
                    continue;
                }

                if (buffered.target.HasValue && buffered.target.Value != player)
                {
                    continue;
                }

                if (buffered.excludeOwner && id.owner == player)
                {
                    continue;
                }

                var battr =
                    buffered.comp < id.behaviours.Count
                    && buffered.method < id.behaviours[buffered.comp].rpcs.Length
                        ? id.behaviours[buffered.comp].rpcs[buffered.method].attr
                        : null;
                var benc =
                    battr != null
                        ? _codec.Encode(
                            RpcKey(buffered.obj, buffered.comp, buffered.method),
                            player.id,
                            battr,
                            buffered.body
                        )
                        : buffered.body;
                Send(
                    player.id,
                    Msg.Rpc,
                    writer =>
                    {
                        writer.WriteBits(buffered.obj, 32);
                        writer.WriteBits(buffered.comp, 8);
                        writer.WriteBits(buffered.method, 8);
                        writer.WriteBits(serverPlayer.Value.id, 16);
                        writer.WriteUInt((ulong)benc.Length, 16);
                        writer.WriteBytes(benc);
                    }
                );
            }
        }

        /// <summary>Server: may this player hold a copy? Owner and the host's own player always may.</summary>
        public bool CanSee(PlayerID player, NetworkIdentity id)
        {
            if (player == localPlayer || id.owner == player)
            {
                return true;
            }

            if (id.visibilityRule != null)
            {
                return id.visibilityRule.CanSee(player, id);
            }

            foreach (var rule in visibilityRules)
            {
                if (!rule.CanSee(player, id))
                {
                    return false;
                }
            }

            return true;
        }

        /// <summary>Server: re-run the rules now; spawns on players that gained sight, despawns on those that lost it.</summary>
        public void EvaluateVisibility()
        {
            if (!isServer)
            {
                return;
            }

            foreach (var id in _ids.Values.ToArray())
            {
                foreach (var player in _players)
                {
                    if (player == localPlayer)
                    {
                        continue;
                    }

                    bool see = CanSee(player, id),
                        has = id.IsObserver(player);
                    if (see && !has)
                    {
                        id.AddObserver(player);
                        Send(player.id, Msg.Spawn, writer => WriteSpawn(writer, id));
                        SendBuffered(player, id);
                    }
                    else if (!see && has)
                    {
                        id.RemoveObserver(player);
                        Send(player.id, Msg.Despawn, writer => writer.WriteBits(id.objectId, 32));
                    }
                }
            }
        }

        // ---- tick ----
        /// <summary>Drives the network: pumps the link, runs ticks and flushes outgoing frames. Called from Update.</summary>
        public void NetworkUpdate(float dt)
        {
            bool connected =
                (isClient && _client != null && _client.State == BridgeState.Started)
                || (isServer && _server != null);
            statistics.SetConnected(connected);
            if (
                statistics.Update(dt)
                && isClient
                && !isServer
                && _client != null
                && _client.State == BridgeState.Started
            )
            {
                Send(
                    -1,
                    Msg.Ping,
                    writer =>
                    {
                        writer.WriteBits((uint)_clock.ElapsedMilliseconds, 32);
                        writer.WriteBits((ulong)Math.Min(65535, Math.Max(0, statistics.ping)), 16);
                    }
                );
            }

            _server?.Pump();
            _client?.Pump();
            if (isOffline)
            {
                _server?.Flush();
                _client?.Flush();
                return;
            }
            PollSceneLoad();
            TickSceneLoad();
            _accum += dt;
            float step = 1f / Math.Max(1, tickRate);
            int count = 0;
            while (_accum >= step && count++ < 8)
            {
                _accum -= step;
                Tick();
            }
            FlushFrames();
            _server?.Flush();
            _client?.Flush();
        }

        void Tick()
        {
            tick++;
            if (isServer)
            {
                serverTick = tick;
            }

            if (isServer && _players.Count > (isClient ? 1 : 0))
            {
                Broadcast(Msg.ServerTick, writer => writer.WriteBits(tick, 32)); // clients learn the server clock even when nothing else changed
            }

            if (
                isClient
                && !isServer
                && serverTick > 0
                && _client != null
                && _client.State == BridgeState.Started
            )
            {
                uint vt = serverTick - (uint)Math.Min(serverTick, (uint)Math.Round(viewDelayTicks));
                Send(-1, Msg.ViewTick, writer => writer.WriteBits(vt, 32));
            }
            if (
                isServer
                && visibilityIntervalTicks > 0
                && tick % (uint)visibilityIntervalTicks == 0
                && (
                    visibilityRules.Count > 0
                    || _ids.Values.Any(identity => identity.visibilityRule != null)
                )
            )
            {
                EvaluateVisibility();
            }

            float now = tick * (1f / Math.Max(1, tickRate));
            foreach (var id in _ids.Values.ToArray())
            {
                if (!id.isSpawned)
                {
                    continue;
                }

                foreach (var behaviour in id.behaviours)
                {
                    for (int mi = 0; mi < behaviour.modules.Length; mi++)
                    {
                        var module = behaviour.modules[mi];
                        if (module.isController)
                        {
                            module.OnBeforeSend();
                        }

                        if (!module.isDirty || !module.isController || now < module.nextSendTime)
                        {
                            continue;
                        }

                        module.nextSendTime = now + module.sendIntervalInSeconds;
                        var body = new BitWriter();
                        module.WriteDelta(body);
                        module.ClearDirty();
                        var bytes = body.Finish();
                        void Write(BitWriter writer)
                        {
                            writer.WriteBits(id.objectId, 32);
                            writer.WriteBits((ulong)behaviour.componentIndex, 8);
                            writer.WriteBits((ulong)mi, 8);
                            writer.WriteBits(tick, 32);
                            writer.WriteUInt((ulong)bytes.Length, 16);
                            writer.WriteBytes(bytes);
                        }
                        if (isServer && module.serverOnly)
                        {
                            module.ClearDirty();
                            continue;
                        }
                        if (isServer)
                        {
                            if (module.ownerOnly && id.owner.HasValue)
                            {
                                if (id.owner != localPlayer)
                                {
                                    Send(id.owner.Value.id, Msg.Sync, Write);
                                }
                            }
                            else
                            {
                                foreach (var player in id.observers)
                                {
                                    if (
                                        id.lod != null
                                        && player != localPlayer
                                        && !id.lod.ShouldSendToPlayer(player, tick)
                                    )
                                    {
                                        continue;
                                    }

                                    Send(player.id, Msg.Sync, Write);
                                }
                            }
                        }
                        else
                        {
                            Send(-1, Msg.Sync, Write);
                        }
                    }
                    behaviour.OnTick(tick);
                }
            }
        }

        // ---- RPC ----
        /// <summary>Routes an RPC call. Returns true when the call was handled here (sent, buffered or refused)
        /// and the body must NOT run locally; false when the caller should execute the body.</summary>
        internal bool SendRpc(
            NetworkBehaviour behaviour,
            RpcMethod method,
            PlayerID? target,
            object[] args,
            Type[] typeArgs = null
        )
        {
            if (!behaviour.isSpawned)
            {
                Debug.LogWarning(
                    $"{behaviour.GetType().Name}.{method.method.Name}: object not spawned"
                );
                return true;
            }
            var id = behaviour.identity;
            switch (method.attr.kind)
            {
                case RpcKind.Server:
                {
                    var attribute = (ServerRpcAttribute)method.attr;
                    if (isServer)
                    {
                        return false;
                    }

                    if (attribute.requireOwnership && !id.isOwner)
                    {
                        Debug.LogWarning(
                            $"{behaviour.GetType().Name}.{method.method.Name}: ServerRpc requires ownership"
                        );
                        return true;
                    }
                    var bytes = _codec.Encode(
                        RpcKey(id, behaviour, method),
                        -1,
                        attribute,
                        Pack(behaviour, method, args, typeArgs)
                    );
                    Send(-1, Msg.Rpc, writer => WriteRpc(writer, id, behaviour, method, bytes));
                    return !attribute.runLocally;
                }
                case RpcKind.Observers:
                {
                    var attribute = (ObserversRpcAttribute)method.attr;
                    if (!isServer)
                    {
                        Debug.LogWarning(
                            $"{behaviour.GetType().Name}.{method.method.Name}: ObserversRpc may only be called on the server"
                        );
                        return true;
                    }
                    var bytes = Pack(behaviour, method, args, typeArgs);
                    var key = RpcKey(id, behaviour, method);
                    foreach (var player in id.observers)
                    {
                        if (player == localPlayer || (attribute.excludeOwner && id.owner == player))
                        {
                            continue;
                        }

                        var enc = _codec.Encode(key, player.id, attribute, bytes);
                        Send(
                            player.id,
                            Msg.Rpc,
                            writer => WriteRpc(writer, id, behaviour, method, enc)
                        );
                    }
                    if (attribute.bufferLast)
                    {
                        Buffer(id, behaviour, method, null, attribute.excludeOwner, bytes);
                    }

                    return !(
                        isClient && attribute.runLocally && !(attribute.excludeOwner && id.isOwner)
                    );
                }
                case RpcKind.Target:
                {
                    var attribute = (TargetRpcAttribute)method.attr;
                    if (!isServer)
                    {
                        Debug.LogWarning(
                            $"{behaviour.GetType().Name}.{method.method.Name}: TargetRpc may only be called on the server"
                        );
                        return true;
                    }
                    if (!target.HasValue)
                    {
                        Debug.LogWarning(
                            $"{behaviour.GetType().Name}.{method.method.Name}: TargetRpc needs a target (first parameter PlayerID, or RpcTo)"
                        );
                        return true;
                    }
                    var bytes = Pack(behaviour, method, args, typeArgs);
                    if (attribute.bufferLast)
                    {
                        Buffer(id, behaviour, method, target, false, bytes);
                    }

                    if (target == localPlayer)
                    {
                        return !(isClient && attribute.runLocally);
                    }

                    if (!id.IsObserver(target.Value))
                    {
                        Debug.LogWarning(
                            $"{behaviour.GetType().Name}.{method.method.Name}: {target} does not see this object"
                        );
                        return true;
                    }
                    {
                        var enc = _codec.Encode(
                            RpcKey(id, behaviour, method),
                            target.Value.id,
                            attribute,
                            bytes
                        );
                        Send(
                            target.Value.id,
                            Msg.Rpc,
                            writer => WriteRpc(writer, id, behaviour, method, enc)
                        );
                    }
                    ;
                    return true;
                }
            }
            return true;
        }

        // ---- static RPCs (woven prologue calls these) ----
        /// <summary>Routes a static RPC through NetworkManager.main. Returns true when the body must NOT run locally.</summary>
        public static bool __StaticRpcSend(uint hash, object[] args)
        {
            var manager = main;
            if (manager == null || (!manager.isServer && !manager.isClient))
            {
                return false;
            }

            var method = StaticRpcs.Find(hash);
            if (method == null)
            {
                return false;
            }

            return manager.SendStaticRpc(method, args);
        }

        public static RpcInfo __StaticLocalInfo() =>
            main == null ? default : new RpcInfo(main.localPlayer ?? default, main.isServer);

        bool SendStaticRpc(StaticRpcMethod method, object[] args)
        {
            string name = method.method.DeclaringType.Name + "." + method.method.Name;
            switch (method.attr.kind)
            {
                case RpcKind.Server:
                {
                    var attribute = (ServerRpcAttribute)method.attr;
                    if (isServer)
                    {
                        return false;
                    }

                    var bytes = PackStatic(method, args);
                    Send(-1, Msg.StaticRpc, writer => WriteStaticRpc(writer, method, bytes));
                    return !attribute.runLocally;
                }
                case RpcKind.Observers:
                {
                    var attribute = (ObserversRpcAttribute)method.attr;
                    if (!isServer)
                    {
                        Debug.LogWarning($"{name}: ObserversRpc may only be called on the server");
                        return true;
                    }
                    var bytes = PackStatic(method, args);
                    foreach (var player in _players)
                    {
                        if (player != localPlayer)
                        {
                            Send(
                                player.id,
                                Msg.StaticRpc,
                                writer => WriteStaticRpc(writer, method, bytes)
                            );
                        }
                    }

                    if (attribute.bufferLast)
                    {
                        _staticBuffered[method.hash] = bytes;
                    }

                    return !(isClient && attribute.runLocally);
                }
                case RpcKind.Target:
                {
                    var attribute = (TargetRpcAttribute)method.attr;
                    if (!isServer)
                    {
                        Debug.LogWarning($"{name}: TargetRpc may only be called on the server");
                        return true;
                    }
                    if (!(args.Length > 0 && args[0] is PlayerID target))
                    {
                        Debug.LogWarning($"{name}: TargetRpc needs a PlayerID first parameter");
                        return true;
                    }
                    if (target == localPlayer)
                    {
                        return !(isClient && attribute.runLocally);
                    }

                    if (!_players.Contains(target))
                    {
                        Debug.LogWarning($"{name}: {target} is not in the room");
                        return true;
                    }
                    var bytes = PackStatic(method, args);
                    Send(target.id, Msg.StaticRpc, writer => WriteStaticRpc(writer, method, bytes));
                    return true;
                }
            }
            return true;
        }

        readonly Dictionary<uint, byte[]> _staticBuffered = new Dictionary<uint, byte[]>();

        static byte[] PackStatic(StaticRpcMethod method, object[] args)
        {
            var body = new BitWriter();
            StaticRpcs.WriteArgs(body, method, args);
            return body.Finish();
        }

        void WriteStaticRpc(BitWriter writer, StaticRpcMethod method, byte[] bytes)
        {
            writer.WriteBits(method.hash, 32);
            writer.WriteBits(localPlayer?.id ?? 0, 16);
            writer.WriteUInt((ulong)bytes.Length, 16);
            writer.WriteBytes(bytes);
        }

        static byte[] Pack(
            NetworkBehaviour behaviour,
            RpcMethod method,
            object[] args,
            Type[] typeArgs = null
        )
        {
            var body = new BitWriter();
            behaviour.WriteArgs(body, method, args, typeArgs);
            return body.Finish();
        }

        void WriteRpc(
            BitWriter writer,
            NetworkIdentity id,
            NetworkBehaviour behaviour,
            RpcMethod method,
            byte[] bytes
        )
        {
            writer.WriteBits(id.objectId, 32);
            writer.WriteBits((ulong)behaviour.componentIndex, 8);
            writer.WriteBits(method.index, 8);
            writer.WriteBits(localPlayer.Value.id, 16);
            writer.WriteUInt((ulong)bytes.Length, 16);
            writer.WriteBytes(bytes);
        }

        void Buffer(
            NetworkIdentity id,
            NetworkBehaviour behaviour,
            RpcMethod rpc,
            PlayerID? target,
            bool excludeOwner,
            byte[] body
        )
        {
            _buffered.RemoveAll(x =>
                x.obj == id.objectId
                && x.comp == behaviour.componentIndex
                && x.method == rpc.index
                && x.target.Equals(target)
            );
            _buffered.Add(
                new Buffered
                {
                    obj = id.objectId,
                    comp = (byte)behaviour.componentIndex,
                    method = rpc.index,
                    target = target,
                    excludeOwner = excludeOwner,
                    body = body,
                }
            );
        }

        // ---- framing ----
        void Send(int actor, Msg kind, Action<BitWriter> write)
        {
            var writer = new BitWriter();
            write(writer);
            var body = writer.Finish();
            var buf =
                actor < 0
                    ? _toServer
                    : (
                        _toClient.TryGetValue(actor, out var bytes)
                            ? bytes
                            : (_toClient[actor] = new List<byte>())
                    );
            buf.Add((byte)kind);
            buf.Add((byte)(body.Length >> 8));
            buf.Add((byte)body.Length);
            buf.AddRange(body);
            statistics.CountOut(body.Length + 3);
        }

        void Broadcast(Msg kind, Action<BitWriter> write, int except = -1)
        {
            if (_server == null)
            {
                return;
            }

            foreach (var actor in _server.Peers)
            {
                if (actor != except)
                {
                    Send(actor, kind, write);
                }
            }
        }

        void FlushFrames()
        {
            if (_server != null)
            {
                foreach (var kv in _toClient)
                {
                    FlushBuffer(kv.Value, bytes => _server.SendTo(kv.Key, bytes, true));
                }
            }
            if (_client != null)
            {
                FlushBuffer(_toServer, bytes => _client.SendToHost(bytes, true));
            }
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
                    if (end + len - pos > mtu && end > pos)
                    {
                        break;
                    }

                    end += len;
                }
                send(new ArraySegment<byte>(buf.GetRange(pos, end - pos).ToArray()));
                pos = end;
            }
            buf.Clear();
        }

        void HandleFrame(ArraySegment<byte> data, PlayerID from, bool asServer)
        {
            statistics.CountIn(data.Count);
            int pos = data.Offset,
                end = data.Offset + data.Count;
            var array = data.Array;
            while (pos + 3 <= end)
            {
                var kind = (Msg)array[pos];
                int len = (array[pos + 1] << 8) | array[pos + 2];
                pos += 3;
                if (pos + len > end)
                {
                    Debug.LogWarning("[AurixNet] truncated frame");
                    return;
                }
                var body = new byte[len];
                Array.Copy(array, pos, body, 0, len);
                pos += len;
                try
                {
                    if (
                        !asServer
                        && _pendingSpawns.Count > 0
                        && (
                            kind == Msg.Owner
                            || kind == Msg.Sync
                            || kind == Msg.Rpc
                            || kind == Msg.Despawn
                        )
                        && len >= 4
                        && _pendingSpawns.TryGetValue(
                            (uint)new BitReader(body).ReadBits(32),
                            out var queue
                        )
                    )
                    {
                        queue.Add((kind, body));
                        continue;
                    }
                    Handle(kind, new BitReader(body), from, asServer);
                }
                catch (Exception error)
                {
                    Debug.LogWarning($"[AurixNet] {kind} from {from}: {error.Message}");
                }
            }
        }

        void Handle(Msg kind, BitReader reader, PlayerID from, bool asServer)
        {
            if (asServer)
            {
                switch (kind)
                {
                    case Msg.SceneReady:
                    {
                        ushort id = (ushort)reader.ReadBits(16);
                        if (id == _sceneLoadId)
                        {
                            SceneReadyFrom(from);
                        }

                        return;
                    }
                    case Msg.StaticRpc:
                    {
                        uint hash = (uint)reader.ReadBits(32);
                        reader.ReadBits(16);
                        var bytes = reader.ReadBytes((int)reader.ReadUInt(16));
                        var sr = StaticRpcs.Find(hash);
                        if (sr == null || sr.attr.kind != RpcKind.Server)
                        {
                            return;
                        }

                        StaticRpcs.Invoke(sr, new BitReader(bytes), new RpcInfo(from, true));
                        return;
                    }
                    case Msg.Ping:
                    {
                        uint stamp = (uint)reader.ReadBits(32);
                        int reported = (int)reader.ReadBits(16);
                        statistics.SetPlayerPing(from, reported);
                        Send(from.id, Msg.Pong, writer => writer.WriteBits(stamp, 32));
                        return;
                    }
                    case Msg.ViewTick:
                    {
                        uint vt = (uint)reader.ReadBits(32);
                        if (vt <= serverTick)
                        {
                            _viewTicks[from] = vt;
                        }

                        return;
                    }
                    case Msg.Hello:
                        if (reader.ReadBits(8) == Version && _players.Contains(from))
                        {
                            SendWelcome(from);
                        }
                        else
                        {
                            Debug.LogWarning(
                                $"[AurixNet] hello from {from} with another protocol version"
                            );
                        }

                        return;
                    case Msg.Sync:
                    {
                        var (id, behaviour, module) = ReadModuleRef(reader);
                        reader.ReadBits(32);
                        var bytes = reader.ReadBytes((int)reader.ReadUInt(16));
                        if (id == null || !module.ownerAuth || id.owner != from)
                        {
                            return;
                        }

                        module.lastReceivedTick = tick;
                        bool accepted = module.ReadDeltaFromOwner(new BitReader(bytes));
                        module.ClearDirty();
                        if (!accepted)
                        {
                            var cw = new BitWriter();
                            module.WriteDelta(cw);
                            var cb = cw.Finish();
                            Send(
                                from.id,
                                Msg.Sync,
                                writer =>
                                {
                                    writer.WriteBits(id.objectId, 32);
                                    writer.WriteBits((ulong)behaviour.componentIndex, 8);
                                    writer.WriteBits((ulong)module.index, 8);
                                    writer.WriteBits(tick, 32);
                                    writer.WriteUInt((ulong)cb.Length, 16);
                                    writer.WriteBytes(cb);
                                }
                            );
                            return;
                        }
                        void Write(BitWriter writer)
                        {
                            writer.WriteBits(id.objectId, 32);
                            writer.WriteBits((ulong)behaviour.componentIndex, 8);
                            writer.WriteBits((ulong)module.index, 8);
                            writer.WriteBits(tick, 32);
                            writer.WriteUInt((ulong)bytes.Length, 16);
                            writer.WriteBytes(bytes);
                        }
                        if (!module.ownerOnly && !module.serverOnly)
                        {
                            foreach (var player in id.observers)
                            {
                                if (player != from)
                                {
                                    Send(player.id, Msg.Sync, Write);
                                }
                            }
                        }

                        return;
                    }
                    case Msg.Rpc:
                    {
                        var (id, behaviour, _) = ReadModuleRef(reader, module: false);
                        byte mi = (byte)reader.ReadBits(8);
                        reader.ReadBits(16);
                        var bytes = reader.ReadBytes((int)reader.ReadUInt(16));
                        if (id == null || mi >= behaviour.rpcs.Length)
                        {
                            return;
                        }

                        var rm = behaviour.rpcs[mi];
                        if (rm.attr.kind != RpcKind.Server)
                        {
                            return;
                        }

                        bytes = _codec.Decode(RpcKey(id, behaviour, rm), from.id, rm.attr, bytes);
                        if (((ServerRpcAttribute)rm.attr).requireOwnership && id.owner != from)
                        {
                            Debug.LogWarning(
                                $"[AurixNet] {behaviour.GetType().Name}.{rm.method.Name} from non-owner {from}"
                            );
                            return;
                        }
                        behaviour.InvokeRpc(rm, new BitReader(bytes), new RpcInfo(from, true));
                        return;
                    }
                }
                return;
            }
            if (from != serverPlayer)
            {
                return;
            }

            switch (kind)
            {
                case Msg.SceneLoad:
                {
                    ushort id = (ushort)reader.ReadBits(16);
                    var name = ReadString(reader);
                    if (isServer)
                    {
                        return;
                    }

                    loadingScene = name;
                    onSceneLoadStart?.Invoke(name);
                    RunSceneLoad(
                        name,
                        () => Send(-1, Msg.SceneReady, writer => writer.WriteBits(id, 16))
                    );
                    return;
                }
                case Msg.SceneReady:
                {
                    reader.ReadBits(16);
                    if (!isServer && loadingScene != null)
                    {
                        var name = loadingScene;
                        loadingScene = null;
                        onSceneLoadDone?.Invoke(name);
                    }
                    return;
                }
                case Msg.Welcome:
                {
                    if (reader.ReadBits(8) != Version)
                    {
                        onError?.Invoke("protocol version mismatch");
                        Stop();
                        return;
                    }
                    tick = serverTick = (uint)reader.ReadBits(32);
                    reader.ReadBits(16);
                    _players.Clear();
                    int count = (int)reader.ReadUInt(16);
                    for (int i = 0; i < count; i++)
                    {
                        _players.Add(new PlayerID((ushort)reader.ReadBits(16)));
                    }

                    return;
                }
                case Msg.StaticRpc:
                {
                    uint hash = (uint)reader.ReadBits(32);
                    var sender = new PlayerID((ushort)reader.ReadBits(16));
                    var bytes = reader.ReadBytes((int)reader.ReadUInt(16));
                    var sr = StaticRpcs.Find(hash);
                    if (sr == null || sr.attr.kind == RpcKind.Server)
                    {
                        return;
                    }

                    StaticRpcs.Invoke(sr, new BitReader(bytes), new RpcInfo(sender, false));
                    return;
                }
                case Msg.Pong:
                {
                    uint stamp = (uint)reader.ReadBits(32);
                    statistics.RecordRtt((uint)_clock.ElapsedMilliseconds - stamp);
                    return;
                }
                case Msg.ServerTick:
                {
                    uint st = (uint)reader.ReadBits(32);
                    if (st > serverTick)
                    {
                        serverTick = st;
                    }

                    return;
                }
                case Msg.PlayerJoined:
                {
                    var player = new PlayerID((ushort)reader.ReadBits(16));
                    if (!_players.Contains(player))
                    {
                        _players.Add(player);
                    }

                    onPlayerJoined?.Invoke(player);
                    return;
                }
                case Msg.PlayerLeft:
                {
                    var player = new PlayerID((ushort)reader.ReadBits(16));
                    _players.Remove(player);
                    onPlayerLeft?.Invoke(player);
                    return;
                }
                case Msg.Spawn:
                {
                    uint oid = (uint)reader.ReadBits(32);
                    uint sid = (uint)reader.ReadBits(32);
                    int pid = (int)reader.ReadUInt(16);
                    if (pid == 0xffff)
                    {
                        pid = -1;
                    }

                    var owner = ReadOwner(reader);
                    if (_ids.ContainsKey(oid))
                    {
                        return;
                    }

                    NetworkIdentity id;
                    void Finish(NetworkIdentity ni)
                    {
                        ni.Setup(this, oid, pid, owner);
                        _ids[oid] = ni;
                        int nb = (int)reader.ReadUInt(8);
                        for (int i = 0; i < nb; i++)
                        {
                            int nm = (int)reader.ReadUInt(8);
                            for (int j = 0; j < nm; j++)
                            {
                                if (i < ni.behaviours.Count && j < ni.behaviours[i].modules.Length)
                                {
                                    var mod = ni.behaviours[i].modules[j];
                                    mod.lastReceivedTick = serverTick;
                                    mod.ReadFull(reader);
                                }
                            }
                        }
                        foreach (var behaviour in ni.behaviours)
                        {
                            foreach (var module in behaviour.modules)
                            {
                                module.ClearDirty();
                            }
                        }

                        ni.MarkSpawned(false);
                        onSpawned?.Invoke(ni);
                    }
                    if (sid != 0)
                    {
                        if (!_sceneObjects.TryGetValue(sid, out id))
                        {
                            Debug.LogWarning($"[AurixNet] unknown scene object {sid}");
                            return;
                        }
                        Finish(id);
                        return;
                    }
                    if (factory is IAsyncNetworkObjectFactory af)
                    {
                        var queue = new List<(Msg, byte[])>();
                        _pendingSpawns[oid] = queue;
                        af.InstantiateAsync(
                            this,
                            pid,
                            ni =>
                            {
                                if (!_pendingSpawns.Remove(oid))
                                {
                                    if (ni != null)
                                    {
                                        factory.Destroy(ni);
                                    }

                                    return;
                                } // stopped meanwhile
                                if (ni == null)
                                {
                                    Debug.LogWarning(
                                        $"[AurixNet] prefab {pid} failed to load; object {oid} dropped"
                                    );
                                    return;
                                }
                                Finish(ni);
                                foreach (var (queuedKind, body) in queue)
                                {
                                    try
                                    {
                                        Handle(queuedKind, new BitReader(body), from, false);
                                    }
                                    catch (Exception error)
                                    {
                                        Debug.LogWarning(
                                            $"[AurixNet] deferred {queuedKind}: {error.Message}"
                                        );
                                    }
                                }
                            }
                        );
                        return;
                    }
                    id = factory.Instantiate(this, pid);
                    Finish(id);
                    return;
                }
                case Msg.Despawn:
                {
                    uint oid = (uint)reader.ReadBits(32);
                    if (_ids.TryGetValue(oid, out var id))
                    {
                        _ids.Remove(oid);
                        id.MarkDespawned();
                        onDespawned?.Invoke(id);
                        if (id.sceneId == 0)
                        {
                            factory.Destroy(id);
                        }
                    }
                    return;
                }
                case Msg.Owner:
                {
                    uint oid = (uint)reader.ReadBits(32);
                    var newOwner = ReadOwner(reader);
                    if (_ids.TryGetValue(oid, out var id))
                    {
                        id.SetOwner(newOwner);
                    }

                    return;
                }
                case Msg.Sync:
                {
                    var (id, _, module) = ReadModuleRef(reader);
                    uint st = (uint)reader.ReadBits(32);
                    var bytes = reader.ReadBytes((int)reader.ReadUInt(16));
                    if (st > serverTick)
                    {
                        serverTick = st;
                    }

                    if (id != null)
                    {
                        module.lastReceivedTick = st;
                        module.ReadDelta(new BitReader(bytes));
                        module.ClearDirty();
                    }
                    return;
                }
                case Msg.Rpc:
                {
                    var (id, behaviour, _) = ReadModuleRef(reader, module: false);
                    byte mi = (byte)reader.ReadBits(8);
                    var sender = new PlayerID((ushort)reader.ReadBits(16));
                    var bytes = reader.ReadBytes((int)reader.ReadUInt(16));
                    if (id == null || mi >= behaviour.rpcs.Length)
                    {
                        return;
                    }

                    var rm = behaviour.rpcs[mi];
                    if (rm.attr.kind == RpcKind.Server)
                    {
                        return;
                    }

                    bytes = _codec.Decode(RpcKey(id, behaviour, rm), -1, rm.attr, bytes);
                    behaviour.InvokeRpc(rm, new BitReader(bytes), new RpcInfo(sender, false));
                    return;
                }
            }
        }

        (NetworkIdentity, NetworkBehaviour, NetworkModule) ReadModuleRef(
            BitReader reader,
            bool module = true
        )
        {
            uint oid = (uint)reader.ReadBits(32);
            int bi = (int)reader.ReadBits(8);
            int mi = module ? (int)reader.ReadBits(8) : -1;
            if (!_ids.TryGetValue(oid, out var id) || bi >= id.behaviours.Count)
            {
                return (null, null, null);
            }

            var behaviour = id.behaviours[bi];
            if (module && mi >= behaviour.modules.Length)
            {
                return (null, null, null);
            }

            return (id, behaviour, module ? behaviour.modules[mi] : null);
        }
    }
}
