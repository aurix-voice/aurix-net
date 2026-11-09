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

namespace Aurix.Net.Transports
{
    public enum BridgeState { Stopped, Starting, Started, Stopping }

    /// <summary>Why a peer or the local side went away; adapters map it onto their own enums.</summary>
    public enum BridgeLeave { Local, Kicked, HostGone, Lost, Failed }

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
    }

    public sealed class RelayBridge : IDisposable
    {
        public const string KickReason = "kicked";

        public readonly RelayBridgeSettings Settings;
        public bool IsHost { get; private set; }
        public BridgeState State { get; private set; } = BridgeState.Stopped;
        public int LocalActor => _client?.Actor ?? -1;
        public int MasterActor { get; private set; } = -1;
        public string LastError { get; private set; }

        /// <summary>Host side: a client actor joined / left the room.</summary>
        public event Action<int> OnPeerJoined;
        public event Action<int, BridgeLeave> OnPeerLeft;
        /// <summary>Frame from actor <c>from</c> (host side: a client; client side: always the master).</summary>
        public event Action<int, ArraySegment<byte>, bool> OnData;
        public event Action<BridgeState> OnState;
        /// <summary>Client side: the link to the host ended (after <see cref="State"/> went Stopped).</summary>
        public event Action<BridgeLeave> OnDisconnected;

        readonly List<int> _peers = new List<int>();
        public IReadOnlyList<int> Peers => _peers;
        public bool HasPeer(int actor) => _peers.Contains(actor);

        ManagedClient _client;

        public RelayBridge(RelayBridgeSettings settings) { Settings = settings; }

        public void StartHost() => Start(true);
        public void StartClient() => Start(false);

        async void Start(bool host)
        {
            if (State != BridgeState.Stopped) return;
            IsHost = host; LastError = null;
            Set(BridgeState.Starting);
            try
            {
                var grant = Settings.GrantProvider != null ? await Settings.GrantProvider() : Settings.Grant;
                if (string.IsNullOrEmpty(grant)) throw new NetException("no grant: set Grant or GrantProvider");
                var c = await ManagedClient.ConnectAsync(Settings.HttpUrl, grant, Settings.TimeoutMs);
                // Frames and control messages can arrive while the join handshake is still awaiting the
                // data path (the host answers player_joined at once); queue them until Started.
                var early = new List<object>(); bool started = false;
                c.OnControl += j => { if (started) OnControl(j); else early.Add(j); };
                c.OnEvent += f =>
                {
                    if (f.Code != Settings.EventCode) return;
                    if (!started) { early.Add(f); return; }
                    Dispatch(f, host);
                };
                if (host) await c.JoinOrCreateAsync(Settings.RoomName, "relay", Settings.MaxPlayers);
                else await c.JoinAsync(Settings.RoomName);
                MasterActor = (int)Json.Num(c.JoinedJson, "master");
                if (host && MasterActor != c.Actor) { c.Dispose(); throw new NetException("room already has a host (master actor " + MasterActor + ")"); }
                if (!host && MasterActor == c.Actor) { c.Dispose(); throw new NetException("no host in room " + Settings.RoomName); }
                _client = c;
                c.OnClosed += r => Fail(BridgeLeave.Lost, r);
                if (host) foreach (var a in Actors(c.JoinedJson)) if (a != c.Actor) AddPeer(a);
                Set(BridgeState.Started);
                started = true;
                foreach (var e in early) { if (e is string j) OnControl(j); else if (e is EventFrame f) Dispatch(f, host); }
                early.Clear();
            }
            catch (Exception e)
            {
                LastError = e.Message;
                Set(BridgeState.Stopped);
                OnDisconnected?.Invoke(BridgeLeave.Failed);
            }
        }

        void Dispatch(EventFrame f, bool host)
        {
            if (host) { if (_peers.Contains(f.From)) OnData?.Invoke(f.From, f.Payload, true); }
            else if (f.From == MasterActor) OnData?.Invoke(f.From, f.Payload, false);
        }

        public void Stop() => Stop(BridgeLeave.Local);

        void Stop(BridgeLeave why)
        {
            if (_client == null) { if (State == BridgeState.Starting) Set(BridgeState.Stopped); return; }
            Set(BridgeState.Stopping);
            if (IsHost) foreach (var a in _peers.ToArray()) { _peers.Remove(a); OnPeerLeft?.Invoke(a, BridgeLeave.Local); }
            try { _client.Dispose(); } catch { }
            _client = null;
            Set(BridgeState.Stopped);
            OnDisconnected?.Invoke(why);
        }

        void Fail(BridgeLeave why, string reason)
        {
            if (State == BridgeState.Stopped || State == BridgeState.Stopping) return;
            LastError = reason;
            Stop(why);
        }

        /// <summary>Host → one client.</summary>
        public void SendTo(int actor, ArraySegment<byte> data, bool reliable)
        {
            if (_client == null || !IsHost) return;
            _client.SendEvent(Settings.EventCode, Copy(data), Target.Actors, reliable, new[] { (ushort)actor });
        }

        /// <summary>Client → host.</summary>
        public void SendToHost(ArraySegment<byte> data, bool reliable)
        {
            if (_client == null || IsHost) return;
            _client.SendEvent(Settings.EventCode, Copy(data), Target.Master, reliable);
        }

        /// <summary>Host: remove a client from the room; it sees <see cref="BridgeLeave.Kicked"/>.</summary>
        public void Kick(int actor)
        {
            if (_client == null || !IsHost) return;
            _ = _client.SendControlAsync("{\"t\":\"kick\",\"actor\":" + actor + ",\"reason\":\"" + KickReason + "\"}");
            RemovePeer(actor, BridgeLeave.Kicked);
        }

        /// <summary>Dispatch pending messages on the calling thread (every frame).</summary>
        public void Pump() => _client?.Poll();
        public void Flush() => _client?.Flush();

        /// <summary>Largest payload for one frame on the given channel.</summary>
        public static int Mtu(bool reliable) => reliable ? 8192 : Packet.MaxUnfragmented - 8;

        public void Dispose() => Stop();

        void OnControl(string j)
        {
            switch (Json.Str(j, "t"))
            {
                case "player_joined": if (IsHost) { var p = Json.Obj(j, "player"); AddPeer((int)Json.Num(p, "actor")); } break;
                case "player_left": if (IsHost) RemovePeer((int)Json.Num(j, "actor"), Json.Str(j, "reason") == KickReason ? BridgeLeave.Kicked : BridgeLeave.Lost); break;
                case "master_changed":
                    if (IsHost) { if ((int)Json.Num(j, "actor") != LocalActor) Fail(BridgeLeave.HostGone, "lost mastership"); }
                    else Fail(BridgeLeave.HostGone, "host left the room");
                    break;
                case "left":
                    Fail(Json.Str(j, "reason") == KickReason ? BridgeLeave.Kicked : BridgeLeave.Lost, Json.Str(j, "reason"));
                    break;
            }
        }

        void AddPeer(int actor) { if (_peers.Contains(actor)) return; _peers.Add(actor); OnPeerJoined?.Invoke(actor); }
        void RemovePeer(int actor, BridgeLeave why) { if (_peers.Remove(actor)) OnPeerLeft?.Invoke(actor, why); }
        void Set(BridgeState s) { State = s; OnState?.Invoke(s); }

        static byte[] Copy(ArraySegment<byte> d) { var b = new byte[d.Count]; Array.Copy(d.Array, d.Offset, b, 0, d.Count); return b; }

        /// <summary>Actor ids from joined.players (array of {actor,...}).</summary>
        static IEnumerable<int> Actors(string joined)
        {
            int i = joined.IndexOf("\"players\"", StringComparison.Ordinal); if (i < 0) yield break;
            int end = joined.IndexOf(']', i); if (end < 0) end = joined.Length;
            for (int k = joined.IndexOf("\"actor\"", i, StringComparison.Ordinal); k >= 0 && k < end; k = joined.IndexOf("\"actor\"", k + 7, StringComparison.Ordinal))
            {
                int s = joined.IndexOf(':', k) + 1; var sb = new StringBuilder();
                while (s < joined.Length && (char.IsDigit(joined[s]) || joined[s] == ' ')) { if (joined[s] != ' ') sb.Append(joined[s]); s++; }
                if (sb.Length > 0) yield return int.Parse(sb.ToString());
            }
        }
    }
}
