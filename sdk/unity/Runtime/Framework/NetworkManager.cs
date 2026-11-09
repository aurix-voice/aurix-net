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
        enum Msg : byte { Welcome = 1, PlayerJoined, PlayerLeft, Spawn, Despawn, Owner, Sync, Rpc }
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
        public INetworkObjectFactory factory = new PrefabFactory();

        public static NetworkManager main { get; private set; }

        public bool isServer { get; private set; }
        public bool isClient { get; private set; }
        public bool isHost => isServer && isClient;
        public bool isOffline => !isServer && !isClient;
        public PlayerID? localPlayer { get; private set; }
        public PlayerID? serverPlayer { get; private set; }
        public uint tick { get; private set; }
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

        RelayBridge _server, _client;
        uint _nextId = 1;
        float _accum;
        readonly List<byte> _toServer = new List<byte>();
        readonly Dictionary<int, List<byte>> _toClient = new Dictionary<int, List<byte>>();
        sealed class Buffered { public uint obj; public byte comp, method; public PlayerID? target; public bool excludeOwner; public byte[] body; }
        readonly List<Buffered> _buffered = new List<Buffered>();

        void Awake() { if (main == null) main = this; }
        void OnDestroy() { Stop(); if (main == this) main = null; }
        void Update() => NetworkUpdate(Time.unscaledDeltaTime);

        RelayBridge NewBridge() => new RelayBridge(new RelayBridgeSettings
        {
            HttpUrl = httpUrl, Grant = grant, GrantProvider = GrantProvider, RoomName = roomName, MaxPlayers = maxPlayers, EventCode = eventCode
        });

        // ---- lifecycle ----
        public void StartHost() => StartServerInternal(true);
        public void StartServer() => StartServerInternal(false);

        void StartServerInternal(bool withLocalPlayer)
        {
            if (_server != null || _client != null) return;
            NetSerializer.ResolveIdentity = Resolve;
            var b = _server = NewBridge();
            b.OnState += s =>
            {
                if (s == BridgeState.Started) { isServer = true; isClient = withLocalPlayer; localPlayer = serverPlayer = new PlayerID((ushort)b.LocalActor); if (withLocalPlayer) _players.Add(localPlayer.Value); }
                onServerState?.Invoke(s);
            };
            b.OnPeerJoined += a => { var p = new PlayerID((ushort)a); _players.Add(p); SendWelcome(p); Broadcast(Msg.PlayerJoined, w => w.WriteBits(p.id, 16), except: a); onPlayerJoined?.Invoke(p); };
            b.OnPeerLeft += (a, why) => { var p = new PlayerID((ushort)a); _players.Remove(p); HandlePlayerLeft(p); Broadcast(Msg.PlayerLeft, w => w.WriteBits(p.id, 16)); onPlayerLeft?.Invoke(p); };
            b.OnData += (from, d, _) => HandleFrame(d, new PlayerID((ushort)from), true);
            b.OnDisconnected += why => { if (why != BridgeLeave.Local) onError?.Invoke(b.LastError); Teardown(); };
            b.StartHost();
        }

        public void StartClient()
        {
            if (_server != null || _client != null) return;
            NetSerializer.ResolveIdentity = Resolve;
            var b = _client = NewBridge();
            b.OnState += s =>
            {
                if (s == BridgeState.Started) { isClient = true; localPlayer = new PlayerID((ushort)b.LocalActor); serverPlayer = new PlayerID((ushort)b.MasterActor); }
                onClientState?.Invoke(s);
            };
            b.OnData += (from, d, _) => HandleFrame(d, new PlayerID((ushort)from), false);
            b.OnDisconnected += why => { if (why != BridgeLeave.Local) onError?.Invoke(b.LastError); Teardown(); };
            b.StartClient();
        }

        public void Stop()
        {
            var s = _server; var c = _client; _server = null; _client = null;
            s?.Stop(); c?.Stop();
            Teardown();
        }

        void Teardown()
        {
            foreach (var id in _ids.Values.ToArray()) { id.MarkDespawned(); onDespawned?.Invoke(id); factory.Destroy(id); }
            _ids.Clear(); _players.Clear(); _buffered.Clear(); _toServer.Clear(); _toClient.Clear();
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
            var id = factory.Instantiate(this, prefabId);
            id.Setup(this, _nextId++, prefabId, owner);
            _ids[id.objectId] = id;
            Broadcast(Msg.Spawn, w => WriteSpawn(w, id));
            foreach (var b in id.behaviours) foreach (var m in b.modules) m.ClearDirty();
            id.MarkSpawned(true);
            onSpawned?.Invoke(id);
            return id;
        }

        public void Despawn(NetworkIdentity id)
        {
            if (!isServer) { Debug.LogWarning("only the server despawns"); return; }
            if (id == null || !_ids.Remove(id.objectId)) return;
            Broadcast(Msg.Despawn, w => w.WriteBits(id.objectId, 32));
            _buffered.RemoveAll(x => x.obj == id.objectId);
            id.MarkDespawned(); onDespawned?.Invoke(id); factory.Destroy(id);
        }

        public void SetOwnership(NetworkIdentity id, PlayerID? player)
        {
            if (!isServer) { Debug.LogWarning("only the server changes ownership"); return; }
            if (id == null || !id.isSpawned || id.owner == player) return;
            if (player.HasValue && !_players.Contains(player.Value)) { Debug.LogWarning($"player {player} is not in the room"); return; }
            id.SetOwner(player);
            Broadcast(Msg.Owner, w => { w.WriteBits(id.objectId, 32); WriteOwner(w, player); });
        }

        void HandlePlayerLeft(PlayerID p)
        {
            foreach (var id in _ids.Values.Where(i => i.owner == p).ToArray())
            {
                if (id.despawnOnOwnerDisconnect) Despawn(id); else SetOwnership(id, null);
            }
        }

        void WriteSpawn(BitWriter w, NetworkIdentity id)
        {
            w.WriteBits(id.objectId, 32); w.WriteUInt((ulong)id.prefabId, 16); WriteOwner(w, id.owner);
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
            foreach (var id in _ids.Values) Send(p.id, Msg.Spawn, w => WriteSpawn(w, id));
            foreach (var b in _buffered)
            {
                if (b.target.HasValue && b.target.Value != p) continue;
                if (b.excludeOwner && _ids.TryGetValue(b.obj, out var ni) && ni.owner == p) continue;
                Send(p.id, Msg.Rpc, w => { w.WriteBits(b.obj, 32); w.WriteBits(b.comp, 8); w.WriteBits(b.method, 8); w.WriteBits(serverPlayer.Value.id, 16); w.WriteUInt((ulong)b.body.Length, 16); w.WriteBytes(b.body); });
            }
        }

        // ---- tick ----
        /// <summary>Drives the network: pumps the link, runs ticks and flushes outgoing frames. Called from Update.</summary>
        public void NetworkUpdate(float dt)
        {
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
            tick++;
            float now = tick * (1f / Math.Max(1, tickRate));
            foreach (var id in _ids.Values.ToArray())
            {
                if (!id.isSpawned) continue;
                foreach (var b in id.behaviours)
                {
                    for (int mi = 0; mi < b.modules.Length; mi++)
                    {
                        var m = b.modules[mi];
                        if (!m.isDirty || !m.isController || now < m.nextSendTime) continue;
                        m.nextSendTime = now + m.sendIntervalInSeconds;
                        var body = new BitWriter(); m.WriteDelta(body); m.ClearDirty();
                        var bytes = body.Finish();
                        void Write(BitWriter w) { w.WriteBits(id.objectId, 32); w.WriteBits((ulong)b.componentIndex, 8); w.WriteBits((ulong)mi, 8); w.WriteUInt((ulong)bytes.Length, 16); w.WriteBytes(bytes); }
                        if (isServer) { if (m.ownerOnly && id.owner.HasValue) Send(id.owner.Value.id, Msg.Sync, Write); else Broadcast(Msg.Sync, Write); }
                        else Send(-1, Msg.Sync, Write);
                    }
                    b.OnTick(tick);
                }
            }
        }

        // ---- RPC ----
        internal void SendRpc(NetworkBehaviour b, RpcMethod r, PlayerID? target, object[] args)
        {
            if (!b.isSpawned) { Debug.LogWarning($"{b.GetType().Name}.{r.method.Name}: object not spawned"); return; }
            var id = b.identity;
            var body = new BitWriter(); b.WriteArgs(body, r, args); var bytes = body.Finish();
            void Write(BitWriter w) { w.WriteBits(id.objectId, 32); w.WriteBits((ulong)b.componentIndex, 8); w.WriteBits(r.index, 8); w.WriteBits(localPlayer.Value.id, 16); w.WriteUInt((ulong)bytes.Length, 16); w.WriteBytes(bytes); }
            var info = new RpcInfo(localPlayer.Value, isServer);
            switch (r.attr.kind)
            {
                case RpcKind.Server:
                {
                    var a = (ServerRpcAttribute)r.attr;
                    if (isServer) { b.InvokeRpcLocal(r, args, info); return; }
                    if (a.requireOwnership && !id.isOwner) { Debug.LogWarning($"{b.GetType().Name}.{r.method.Name}: ServerRpc requires ownership"); return; }
                    Send(-1, Msg.Rpc, Write);
                    if (a.runLocally) b.InvokeRpcLocal(r, args, info);
                    return;
                }
                case RpcKind.Observers:
                {
                    var a = (ObserversRpcAttribute)r.attr;
                    if (!isServer) { Debug.LogWarning($"{b.GetType().Name}.{r.method.Name}: ObserversRpc may only be called on the server"); return; }
                    foreach (var p in _players) { if (p == localPlayer || (a.excludeOwner && id.owner == p)) continue; Send(p.id, Msg.Rpc, Write); }
                    if (a.bufferLast) Buffer(id, b, r, null, a.excludeOwner, bytes);
                    if (isClient && a.runLocally && !(a.excludeOwner && id.isOwner)) b.InvokeRpcLocal(r, args, info);
                    return;
                }
                case RpcKind.Target:
                {
                    var a = (TargetRpcAttribute)r.attr;
                    if (!isServer) { Debug.LogWarning($"{b.GetType().Name}.{r.method.Name}: TargetRpc may only be called on the server"); return; }
                    if (!target.HasValue) { Debug.LogWarning($"{b.GetType().Name}.{r.method.Name}: TargetRpc needs a target (use RpcTo)"); return; }
                    if (a.bufferLast) Buffer(id, b, r, target, false, bytes);
                    if (target == localPlayer) { if (isClient && a.runLocally) b.InvokeRpcLocal(r, args, info); return; }
                    Send(target.Value.id, Msg.Rpc, Write);
                    return;
                }
            }
        }

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
                    case Msg.Sync: { var (id, b, m) = ReadModuleRef(r); var bytes = r.ReadBytes((int)r.ReadUInt(16)); if (id == null || !m.ownerAuth || id.owner != from) return; m.ReadDelta(new BitReader(bytes)); m.ClearDirty();
                        void Write(BitWriter w) { w.WriteBits(id.objectId, 32); w.WriteBits((ulong)b.componentIndex, 8); w.WriteBits((ulong)m.index, 8); w.WriteUInt((ulong)bytes.Length, 16); w.WriteBytes(bytes); }
                        if (!m.ownerOnly) Broadcast(Msg.Sync, Write, except: from.id); return; }
                    case Msg.Rpc:
                    {
                        var (id, b, _) = ReadModuleRef(r, module: false); byte mi = (byte)r.ReadBits(8); r.ReadBits(16); var bytes = r.ReadBytes((int)r.ReadUInt(16));
                        if (id == null || mi >= b.rpcs.Length) return;
                        var rm = b.rpcs[mi]; if (rm.attr.kind != RpcKind.Server) return;
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
                    tick = (uint)r.ReadBits(32); r.ReadBits(16);
                    _players.Clear(); int n = (int)r.ReadUInt(16); for (int i = 0; i < n; i++) _players.Add(new PlayerID((ushort)r.ReadBits(16)));
                    return;
                }
                case Msg.PlayerJoined: { var p = new PlayerID((ushort)r.ReadBits(16)); if (!_players.Contains(p)) _players.Add(p); onPlayerJoined?.Invoke(p); return; }
                case Msg.PlayerLeft: { var p = new PlayerID((ushort)r.ReadBits(16)); _players.Remove(p); onPlayerLeft?.Invoke(p); return; }
                case Msg.Spawn:
                {
                    uint oid = (uint)r.ReadBits(32); int pid = (int)r.ReadUInt(16); var owner = ReadOwner(r);
                    if (_ids.ContainsKey(oid)) return;
                    var id = factory.Instantiate(this, pid); id.Setup(this, oid, pid, owner); _ids[oid] = id;
                    int nb = (int)r.ReadUInt(8);
                    for (int i = 0; i < nb; i++) { int nm = (int)r.ReadUInt(8); for (int j = 0; j < nm; j++) if (i < id.behaviours.Count && j < id.behaviours[i].modules.Length) id.behaviours[i].modules[j].ReadFull(r); }
                    foreach (var b in id.behaviours) foreach (var m in b.modules) m.ClearDirty();
                    id.MarkSpawned(false); onSpawned?.Invoke(id);
                    return;
                }
                case Msg.Despawn:
                {
                    uint oid = (uint)r.ReadBits(32);
                    if (_ids.TryGetValue(oid, out var id)) { _ids.Remove(oid); id.MarkDespawned(); onDespawned?.Invoke(id); factory.Destroy(id); }
                    return;
                }
                case Msg.Owner: { uint oid = (uint)r.ReadBits(32); var o = ReadOwner(r); if (_ids.TryGetValue(oid, out var id)) id.SetOwner(o); return; }
                case Msg.Sync: { var (id, _, m) = ReadModuleRef(r); var bytes = r.ReadBytes((int)r.ReadUInt(16)); if (id != null) { m.ReadDelta(new BitReader(bytes)); m.ClearDirty(); } return; }
                case Msg.Rpc:
                {
                    var (id, b, _) = ReadModuleRef(r, module: false); byte mi = (byte)r.ReadBits(8); var sender = new PlayerID((ushort)r.ReadBits(16)); var bytes = r.ReadBytes((int)r.ReadUInt(16));
                    if (id == null || mi >= b.rpcs.Length) return;
                    var rm = b.rpcs[mi]; if (rm.attr.kind == RpcKind.Server) return;
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
