// FishNet transport over an Aurix Net relay room (see Transports/RelayBridge.cs for the topology).
// Compiled only when FishNet is in the project (FishNet defines FISHNET).
#if FISHNET
using System;
using System.Threading.Tasks;
using Aurix.Net.Transports;
using FishNet.Managing;
using FishNet.Transporting;
using UnityEngine;
using FChannel = FishNet.Transporting.Channel;

namespace Aurix.Net.FishNetTransport
{
    public sealed class AurixNetFishNetTransport : Transport
    {
        [Header("Aurix Net")]
        [Tooltip("Node control-plane URL, e.g. https://node.example.com")]
        public string httpUrl = "http://127.0.0.1:8090";
        [Tooltip("Grant issued by your backend (POST /v1/grants). Leave empty and set GrantProvider from code.")]
        public string grant = "";
        [Tooltip("Relay room name shared by host and clients. SetClientAddress overrides it when non-empty.")]
        public string roomName = "fishnet";
        public int maxPlayers = 16;
        [Tooltip("Relay event code reserved for FishNet frames.")]
        public byte eventCode = 200;

        /// <summary>Async grant source; wins over the inspector field.</summary>
        public Func<Task<string>> GrantProvider;

        public override event Action<ClientConnectionStateArgs> OnClientConnectionState;
        public override event Action<ServerConnectionStateArgs> OnServerConnectionState;
        public override event Action<RemoteConnectionStateArgs> OnRemoteConnectionState;
        public override event Action<ClientReceivedDataArgs> OnClientReceivedData;
        public override event Action<ServerReceivedDataArgs> OnServerReceivedData;

        RelayBridge _server, _client;
        string _clientRoom;

        RelayBridge NewBridge(string room) => new RelayBridge(new RelayBridgeSettings
        {
            HttpUrl = httpUrl, Grant = grant, GrantProvider = GrantProvider, RoomName = room, MaxPlayers = maxPlayers, EventCode = eventCode
        });

        public override string GetConnectionAddress(int connectionId) => "actor:" + connectionId;

        public override void HandleClientConnectionState(ClientConnectionStateArgs a) => OnClientConnectionState?.Invoke(a);
        public override void HandleServerConnectionState(ServerConnectionStateArgs a) => OnServerConnectionState?.Invoke(a);
        public override void HandleRemoteConnectionState(RemoteConnectionStateArgs a) => OnRemoteConnectionState?.Invoke(a);
        public override void HandleClientReceivedDataArgs(ClientReceivedDataArgs a) => OnClientReceivedData?.Invoke(a);
        public override void HandleServerReceivedDataArgs(ServerReceivedDataArgs a) => OnServerReceivedData?.Invoke(a);

        public override LocalConnectionState GetConnectionState(bool server)
        {
            var b = server ? _server : _client;
            return b == null ? LocalConnectionState.Stopped : Map(b.State);
        }

        public override RemoteConnectionState GetConnectionState(int connectionId) =>
            _server != null && _server.HasPeer(connectionId) ? RemoteConnectionState.Started : RemoteConnectionState.Stopped;

        public override void SendToServer(byte channelId, ArraySegment<byte> segment) => _client?.SendToHost(segment, channelId == (byte)FChannel.Reliable);
        public override void SendToClient(byte channelId, ArraySegment<byte> segment, int connectionId) => _server?.SendTo(connectionId, segment, channelId == (byte)FChannel.Reliable);

        public override void IterateIncoming(bool asServer) { if (asServer) _server?.Pump(); else _client?.Pump(); }
        public override void IterateOutgoing(bool asServer) { if (asServer) _server?.Flush(); else _client?.Flush(); }

        public override void SetClientAddress(string address) => _clientRoom = address;
        public override string GetClientAddress() => string.IsNullOrEmpty(_clientRoom) ? roomName : _clientRoom;
        public override int GetMaximumClients() => maxPlayers - 1;
        public override void SetMaximumClients(int value) => maxPlayers = value + 1;

        public override bool StartConnection(bool server) => server ? StartServer() : StartClient();

        bool StartServer()
        {
            if (_server != null) return false;
            var b = _server = NewBridge(roomName);
            b.OnState += s => HandleServerConnectionState(new ServerConnectionStateArgs(Map(s), Index));
            b.OnPeerJoined += a => HandleRemoteConnectionState(new RemoteConnectionStateArgs(RemoteConnectionState.Started, a, Index));
            b.OnPeerLeft += (a, why) => HandleRemoteConnectionState(new RemoteConnectionStateArgs(RemoteConnectionState.Stopped, a, Index));
            b.OnData += (from, d, _) => HandleServerReceivedDataArgs(new ServerReceivedDataArgs(d, FChannel.Reliable, from, Index));
            b.OnDisconnected += why => { if (_server == b) _server = null; if (why != BridgeLeave.Local) Debug.LogWarning("[AurixNet] server stopped: " + b.LastError); };
            b.StartHost();
            return true;
        }

        bool StartClient()
        {
            if (_client != null) return false;
            var b = _client = NewBridge(GetClientAddress());
            b.OnState += s => HandleClientConnectionState(new ClientConnectionStateArgs(Map(s), Index));
            b.OnData += (from, d, _) => HandleClientReceivedDataArgs(new ClientReceivedDataArgs(d, FChannel.Reliable, Index));
            b.OnDisconnected += why => { if (_client == b) _client = null; if (why == BridgeLeave.Failed) Debug.LogError("[AurixNet] connect failed: " + b.LastError); };
            b.StartClient();
            return true;
        }

        public override bool StopConnection(bool server)
        {
            var b = server ? _server : _client;
            if (b == null) return false;
            if (server) _server = null; else _client = null;
            b.Stop();
            return true;
        }

        public override bool StopConnection(int connectionId, bool immediately)
        {
            if (_server == null || !_server.HasPeer(connectionId)) return false;
            _server.Kick(connectionId);
            return true;
        }

        public override void Shutdown() { StopConnection(false); StopConnection(true); }

        public override int GetMTU(byte channel) => RelayBridge.Mtu(channel == (byte)FChannel.Reliable);

        static LocalConnectionState Map(BridgeState s) => s switch
        {
            BridgeState.Starting => LocalConnectionState.Starting,
            BridgeState.Started => LocalConnectionState.Started,
            BridgeState.Stopping => LocalConnectionState.Stopping,
            _ => LocalConnectionState.Stopped
        };
    }
}
#endif
