// Shared core for the third-party netcode transports (PurrNet, Mirror, FishNet): one Aurix relay room
// carries a host (the room master) and its clients. Host→client frames are events targeted at one
// actor, client→host frames are events to the master, PurrNet/Mirror/FishNet connection ids are the
// Aurix actor ids. Plain C#, no engine dependency; the adapters are thin MonoBehaviour wrappers.
using System;
using System.Collections.Generic;
using System.Text;
using System.Threading.Tasks;
using Aurix.Net;
using Aurix.Net.Managed;
using Newtonsoft.Json.Linq;

namespace Aurix.Net.Transports
{
    public enum BridgeState
    {
        Stopped,
        Starting,
        Started,
        Stopping,
    }

    /// <summary>Why a peer or the local side went away; adapters map it onto their own enums.</summary>
    public enum BridgeLeave
    {
        Local,
        Kicked,
        HostGone,
        Lost,
        Failed,
    }

    public sealed class RelayBridgeSettings
    {
        public string HttpUrl = "http://127.0.0.1:8090";
        public string Grant = "";

        /// <summary>Async grant source; wins over <see cref="Grant"/>.</summary>
        public Func<Task<string>> GrantProvider;
        public string RoomName = "game";
        public int MaxPlayers = 16;

        /// <summary>Relay event code reserved for transport frames; keep other events off it.</summary>
        public byte EventCode = 200;
        public int TimeoutMs = 10000;

        /// <summary>Keep the link when the master changes: raise OnPromoted / OnHostChanged instead of dropping.</summary>
        public bool AllowHostMigration;

        /// <summary>StartAuto: join_random filter (room props that must match exactly, JSON object) and SQL-like lobby expression.</summary>
        public string MatchFilterJson,
            MatchExpr;

        /// <summary>Props the room is created with when hosting (JSON object); StartAuto matches them against MatchFilterJson.</summary>
        public string PropsJson;
    }

    public sealed class RelayBridge : IDisposable
    {
        public const string KickReason = "kicked";

        public readonly RelayBridgeSettings Settings;
        public bool IsHost { get; private set; }
        public BridgeState State { get; private set; } = BridgeState.Stopped;
        public int LocalActor => _localHost != null ? _localHost.LocalActor : _client?.Actor ?? -1;
        public int MasterActor { get; private set; } = -1;
        public string LastError { get; private set; }

        /// <summary>Host side: a client actor joined / left the room.</summary>
        public event Action<int> OnPeerJoined;
        public event Action<int, BridgeLeave> OnPeerLeft;

        /// <summary>Frame from actor <c>from</c> (host side: a client; client side: always the master); the flag is true when it came on the reliable channel.</summary>
        public event Action<int, ArraySegment<byte>, bool> OnData;
        public event Action<BridgeState> OnState;

        /// <summary>Client side: the link to the host ended (after <see cref="State"/> went Stopped).</summary>
        public event Action<BridgeLeave> OnDisconnected;

        /// <summary>Host migration: this peer became the room master (now the host).</summary>
        public event Action OnPromoted;

        /// <summary>Host migration: another peer became the master (clients keep their link).</summary>
        public event Action<int> OnHostChanged;

        readonly List<int> _peers = new List<int>();
        readonly List<int> _known = new List<int>();
        public IReadOnlyList<int> Peers => _peers;

        public bool HasPeer(int actor) => _peers.Contains(actor);

        ManagedClient _client;

        // Host mode: the client half of an adapter is linked to the host bridge in this process and
        // shows up on the host as the host's own actor id. Frames wait in _local until Pump.
        RelayBridge _localHost,
            _localClient;
        readonly Queue<KeyValuePair<byte[], bool>> _local = new Queue<KeyValuePair<byte[], bool>>();
        int _run;

        public RelayBridge(RelayBridgeSettings settings)
        {
            Settings = settings;
        }

        public void StartHost() => Start(true);

        public void StartClient() => Start(false);

        /// <summary>Quick match: join a random open room matching MatchFilterJson/MatchExpr as a client, or host a fresh one (random name, PropsJson) when none exists. Check IsHost once Started.</summary>
        public void StartAuto() => Start(false, true);

        /// <summary>Host mode: connect to a host bridge of this process instead of joining the room a second time. The host sees its own actor id as a peer.</summary>
        public void StartLocal(RelayBridge host)
        {
            if (State != BridgeState.Stopped)
            {
                return;
            }

            IsHost = false;
            LastError = null;
            Set(BridgeState.Starting);
            if (
                host == null
                || host._localClient != null
                || host.State == BridgeState.Stopped
                || host.State == BridgeState.Stopping
            )
            {
                LastError = "no local host";
                Set(BridgeState.Stopped);
                OnDisconnected?.Invoke(BridgeLeave.Failed);
                return;
            }
            _localHost = host;
            host._localClient = this;
            if (host.State == BridgeState.Started)
            {
                host.AttachLocal();
            }
        }

        void AttachLocal()
        {
            var client = _localClient;
            if (client == null || !IsHost || client.State != BridgeState.Starting)
            {
                return;
            }

            client.MasterActor = LocalActor;
            _peers.Add(LocalActor);
            OnPeerJoined?.Invoke(LocalActor);
            if (_localClient == client)
            {
                client.Set(BridgeState.Started);
            }
        }

        void EndLocal(BridgeLeave why, string reason)
        {
            var host = _localHost;
            if (host == null)
            {
                return;
            }

            int actor = host.LocalActor;
            _localHost = null;
            _local.Clear();
            if (why != BridgeLeave.Local)
            {
                LastError = reason;
            }

            Set(BridgeState.Stopping);
            if (host._localClient == this)
            {
                host._localClient = null;
                host._local.Clear();
                host.RemovePeer(
                    actor,
                    why == BridgeLeave.Kicked ? BridgeLeave.Kicked : BridgeLeave.Lost
                );
            }
            Set(BridgeState.Stopped);
            OnDisconnected?.Invoke(why);
        }

        async void Start(bool host, bool auto = false)
        {
            if (State != BridgeState.Stopped)
            {
                return;
            }

            IsHost = host;
            LastError = null;
            int run = ++_run;
            Set(BridgeState.Starting);
            try
            {
                var grant =
                    Settings.GrantProvider != null
                        ? await Settings.GrantProvider()
                        : Settings.Grant;
                if (string.IsNullOrEmpty(grant))
                {
                    throw new NetException("no grant: set Grant or GrantProvider");
                }

                var client = await ManagedClient.ConnectAsync(
                    Settings.HttpUrl,
                    grant,
                    Settings.TimeoutMs
                );
                // Stopped while connecting: drop the link instead of joining.
                if (run != _run)
                {
                    client.Dispose();
                    return;
                }
                // Frames and control messages can arrive while the join handshake is still awaiting the
                // data path (the host answers player_joined at once); queue them until Started.
                var early = new List<object>();
                bool started = false;
                client.OnControl += json =>
                {
                    if (started)
                    {
                        OnControl(json);
                    }
                    else
                    {
                        early.Add(json);
                    }
                };
                client.OnEvent += frame =>
                {
                    if (frame.Code != Settings.EventCode)
                    {
                        return;
                    }

                    if (!started)
                    {
                        early.Add(frame);
                        return;
                    }
                    Dispatch(frame, host);
                };
                if (auto)
                {
                    try
                    {
                        await client.JoinRandomAsync(Settings.MatchFilterJson, Settings.MatchExpr);
                    }
                    catch (NetException error) when (error.Message.Contains("no matching room"))
                    {
                        Settings.RoomName = "m-" + Guid.NewGuid().ToString("N").Substring(0, 10);
                        host = IsHost = true;
                        await client.JoinOrCreateAsync(
                            Settings.RoomName,
                            "relay",
                            Settings.MaxPlayers,
                            Settings.PropsJson
                        );
                    }
                }
                else if (host)
                {
                    await client.JoinOrCreateAsync(
                        Settings.RoomName,
                        "relay",
                        Settings.MaxPlayers,
                        Settings.PropsJson
                    );
                }
                else
                {
                    await client.JoinAsync(Settings.RoomName);
                }

                if (run != _run)
                {
                    client.Dispose();
                    return;
                }
                MasterActor = (int)Json.Num(client.JoinedJson, "master");
                if (host && MasterActor != client.Actor)
                {
                    client.Dispose();
                    throw new NetException(
                        "room already has a host (master actor " + MasterActor + ")"
                    );
                }
                if (!host && MasterActor == client.Actor)
                {
                    client.Dispose();
                    throw new NetException("no host in room " + Settings.RoomName);
                }
                _client = client;
                client.OnClosed += result => Fail(BridgeLeave.Lost, result);
                foreach (var actor in Actors(client.JoinedJson))
                {
                    if (actor != client.Actor && !_known.Contains(actor))
                    {
                        _known.Add(actor);
                    }

                    if (host && actor != client.Actor)
                    {
                        AddPeer(actor);
                    }
                }
                Set(BridgeState.Started);
                started = true;
                foreach (var error in early)
                {
                    if (error is string json)
                    {
                        OnControl(json);
                    }
                    else if (error is EventFrame frame)
                    {
                        Dispatch(frame, host);
                    }
                }
                early.Clear();
                AttachLocal();
            }
            catch (Exception error)
            {
                if (run != _run)
                {
                    return;
                }

                LastError = error.Message;
                _localClient?.EndLocal(BridgeLeave.Failed, error.Message);
                Set(BridgeState.Stopped);
                OnDisconnected?.Invoke(BridgeLeave.Failed);
            }
        }

        void Dispatch(EventFrame frame, bool host)
        {
            if (host)
            {
                if (_peers.Contains(frame.From))
                {
                    OnData?.Invoke(frame.From, frame.Payload, frame.Reliable);
                }
            }
            else if (frame.From == MasterActor)
            {
                OnData?.Invoke(frame.From, frame.Payload, frame.Reliable);
            }
        }

        public void Stop() => Stop(BridgeLeave.Local);

        void Stop(BridgeLeave why)
        {
            if (_localHost != null)
            {
                EndLocal(why, LastError);
                return;
            }
            _run++;
            if (_client == null)
            {
                if (State != BridgeState.Starting)
                {
                    return;
                }

                _localClient?.EndLocal(BridgeLeave.HostGone, "host stopped");
                Set(BridgeState.Stopped);
                return;
            }
            Set(BridgeState.Stopping);
            if (IsHost)
            {
                foreach (var actor in _peers.ToArray())
                {
                    _peers.Remove(actor);
                    OnPeerLeft?.Invoke(actor, BridgeLeave.Local);
                }
            }

            _localClient?.EndLocal(BridgeLeave.HostGone, "host stopped");
            try
            {
                _client.Dispose();
            }
            catch { }
            _client = null;
            Set(BridgeState.Stopped);
            OnDisconnected?.Invoke(why);
        }

        void Fail(BridgeLeave why, string reason)
        {
            if (State == BridgeState.Stopped || State == BridgeState.Stopping)
            {
                return;
            }

            LastError = reason;
            Stop(why);
        }

        /// <summary>Host → one client.</summary>
        public void SendTo(int actor, ArraySegment<byte> data, bool reliable)
        {
            if (_client == null || !IsHost)
            {
                return;
            }

            if (_localClient != null && actor == LocalActor)
            {
                if (_localClient.State == BridgeState.Started)
                {
                    _localClient._local.Enqueue(
                        new KeyValuePair<byte[], bool>(Copy(data), reliable)
                    );
                }

                return;
            }
            _client.SendEvent(
                Settings.EventCode,
                Copy(data),
                Target.Actors,
                reliable,
                new[] { (ushort)actor }
            );
        }

        /// <summary>Client → host.</summary>
        public void SendToHost(ArraySegment<byte> data, bool reliable)
        {
            if (_localHost != null)
            {
                if (State == BridgeState.Started)
                {
                    _localHost._local.Enqueue(new KeyValuePair<byte[], bool>(Copy(data), reliable));
                }

                return;
            }
            if (_client == null || IsHost)
            {
                return;
            }

            _client.SendEvent(Settings.EventCode, Copy(data), Target.Master, reliable);
        }

        /// <summary>Host: remove a client from the room; it sees <see cref="BridgeLeave.Kicked"/>.</summary>
        public void Kick(int actor)
        {
            if (_client == null || !IsHost)
            {
                return;
            }

            if (_localClient != null && actor == LocalActor)
            {
                _localClient.EndLocal(BridgeLeave.Kicked, KickReason);
                return;
            }
            _ = _client.SendControlAsync(
                "{\"t\":\"kick\",\"actor\":" + actor + ",\"reason\":\"" + KickReason + "\"}"
            );
            RemovePeer(actor, BridgeLeave.Kicked);
        }

        /// <summary>Dispatch pending messages on the calling thread (every frame).</summary>
        public void Pump()
        {
            _client?.Poll();
            while (_local.Count > 0 && State == BridgeState.Started)
            {
                var frame = _local.Dequeue();
                OnData?.Invoke(
                    _localHost != null ? MasterActor : LocalActor,
                    new ArraySegment<byte>(frame.Key),
                    frame.Value
                );
            }
        }

        public void Flush() => _client?.Flush();

        /// <summary>Largest payload for one frame on the given channel.</summary>
        public static int Mtu(bool reliable) => reliable ? 8192 : Packet.MaxUnfragmented - 8;

        public void Dispose() => Stop();

        void OnControl(string json)
        {
            switch (Json.Str(json, "t"))
            {
                case "player_joined":
                {
                    var player = Json.Obj(json, "player");
                    int actor = (int)Json.Num(player, "actor");
                    if (IsHost)
                    {
                        AddPeer(actor);
                    }
                    else if (!_known.Contains(actor))
                    {
                        _known.Add(actor);
                    }

                    break;
                }
                case "player_left":
                {
                    int actor = (int)Json.Num(json, "actor");
                    _known.Remove(actor);
                    if (IsHost)
                    {
                        RemovePeer(
                            actor,
                            Json.Str(json, "reason") == KickReason
                                ? BridgeLeave.Kicked
                                : BridgeLeave.Lost
                        );
                    }

                    break;
                }
                case "master_changed":
                {
                    int master = (int)Json.Num(json, "actor");
                    if (!Settings.AllowHostMigration)
                    {
                        if (!IsHost || master != LocalActor)
                        {
                            Fail(
                                BridgeLeave.HostGone,
                                IsHost ? "lost mastership" : "host left the room"
                            );
                        }

                        break;
                    }
                    MasterActor = master;
                    if (master == LocalActor && !IsHost)
                    {
                        IsHost = true;
                        _peers.Clear();
                        foreach (var actor in _known)
                        {
                            if (actor != LocalActor)
                            {
                                _peers.Add(actor);
                            }
                        }

                        OnPromoted?.Invoke();
                    }
                    else if (master != LocalActor)
                    {
                        if (IsHost)
                        {
                            _localClient?.EndLocal(BridgeLeave.HostGone, "lost mastership");
                            IsHost = false;
                            _peers.Clear();
                        }
                        OnHostChanged?.Invoke(master);
                    }
                    break;
                }
                case "left":
                    Fail(
                        Json.Str(json, "reason") == KickReason
                            ? BridgeLeave.Kicked
                            : BridgeLeave.Lost,
                        Json.Str(json, "reason")
                    );
                    break;
            }
        }

        void AddPeer(int actor)
        {
            if (!_known.Contains(actor))
            {
                _known.Add(actor);
            }

            if (_peers.Contains(actor))
            {
                return;
            }

            _peers.Add(actor);
            OnPeerJoined?.Invoke(actor);
        }

        void RemovePeer(int actor, BridgeLeave why)
        {
            if (_peers.Remove(actor))
            {
                OnPeerLeft?.Invoke(actor, why);
            }
        }

        void Set(BridgeState state)
        {
            State = state;
            OnState?.Invoke(state);
        }

        static byte[] Copy(ArraySegment<byte> data)
        {
            var bytes = new byte[data.Count];
            Array.Copy(data.Array, data.Offset, bytes, 0, data.Count);
            return bytes;
        }

        /// <summary>Actor ids from joined.players (array of {actor,...}).</summary>
        static IEnumerable<int> Actors(string joined)
        {
            if (!(Json.Parse(joined)?["players"] is JArray players))
            {
                yield break;
            }

            foreach (var player in players)
            {
                if (player["actor"] is JValue { Type: JTokenType.Integer } actor)
                {
                    yield return (int)actor;
                }
            }
        }
    }
}
