// PurrNet transport over an Aurix Net relay room (see Transports/RelayBridge.cs for the topology).
// Compiled only when the PurrNet package is present (AURIX_PURRNET is set by the asmdef version define).
#if AURIX_PURRNET
using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Aurix.Net.Transports;
using PurrNet.Transports;
using UnityEngine;
using PChannel = PurrNet.Transports.Channel;

namespace Aurix.Net.PurrNet
{
    public sealed class AurixNetTransport : GenericTransport, ITransport
    {
        [Header("Aurix Net")]
        [Tooltip("Node control-plane URL, e.g. https://node.example.com")]
        public string httpUrl = "http://127.0.0.1:8090";
        [Tooltip("Grant issued by your backend (POST /v1/grants). Leave empty and set GrantProvider from code.")]
        public string grant = "";
        [Tooltip("Relay room name shared by host and clients.")]
        public string roomName = "purrnet";
        public int maxPlayers = 16;
        [Tooltip("Relay event code reserved for PurrNet frames.")]
        public byte eventCode = 200;

        /// <summary>Async grant source; wins over the inspector field.</summary>
        public Func<Task<string>> GrantProvider;

        public override bool isSupported => true;
        public override ITransport transport => this;

        public event OnConnected onConnected;
        public event OnDisconnected onDisconnected;
        public event OnDataReceived onDataReceived;
        public event OnDataSent onDataSent;
        public event OnConnectionState onConnectionState;

        readonly List<Connection> _connections = new List<Connection>();
        public IReadOnlyList<Connection> connections => _connections;

        public ConnectionState listenerState { get; private set; } = ConnectionState.Disconnected;
        public ConnectionState clientState { get; private set; } = ConnectionState.Disconnected;

        RelayBridge _host, _client;
        static readonly Connection ServerConn = new Connection(0);

        RelayBridge NewBridge() => new RelayBridge(new RelayBridgeSettings
        {
            HttpUrl = httpUrl, Grant = grant, GrantProvider = GrantProvider, RoomName = roomName, MaxPlayers = maxPlayers, EventCode = eventCode
        });

        protected override void StartClientInternal() => Connect(null, 0);
        protected override void StartServerInternal() => Listen(0);

        // ---- IListener (PurrNet server = room master) ----
        public void Listen(ushort port)
        {
            if (_host != null) return;
            var b = _host = NewBridge();
            b.OnState += s => { listenerState = Map(s); onConnectionState?.Invoke(listenerState, true); };
            b.OnPeerJoined += a => { var c = new Connection(a); _connections.Add(c); onConnected?.Invoke(c, true); };
            b.OnPeerLeft += (a, why) => { var c = new Connection(a); if (_connections.Remove(c)) onDisconnected?.Invoke(c, Map(why, true), true); };
            b.OnData += (from, d, _) => onDataReceived?.Invoke(new Connection(from), new ByteData(d.Array, d.Offset, d.Count), true);
            b.OnDisconnected += why => { if (why != BridgeLeave.Local) Debug.LogWarning("[AurixNet] host stopped: " + b.LastError); if (_host == b) _host = null; };
            b.StartHost();
        }

        public void StopListening() { var b = _host; _host = null; b?.Stop(); }

        // ---- IConnectable (PurrNet client) ----
        public void Connect(string ip, ushort port)
        {
            if (_client != null) return;
            var b = _client = NewBridge();
            b.OnState += s =>
            {
                clientState = Map(s); onConnectionState?.Invoke(clientState, false);
                if (s == BridgeState.Started) onConnected?.Invoke(ServerConn, false);
            };
            b.OnData += (from, d, _) => onDataReceived?.Invoke(ServerConn, new ByteData(d.Array, d.Offset, d.Count), false);
            b.OnDisconnected += why =>
            {
                if (why == BridgeLeave.Failed) Debug.LogError("[AurixNet] connect failed: " + b.LastError);
                if (_client == b) _client = null;
                onDisconnected?.Invoke(ServerConn, Map(why, false), false);
            };
            b.StartClient();
        }

        public void Disconnect() { var b = _client; _client = null; b?.Stop(); }

        // ---- data ----
        public void SendToClient(Connection target, ByteData data, PChannel method = PChannel.ReliableOrdered)
        {
            if (_host == null || !target.isValid) return;
            _host.SendTo(target.connectionId, data.segment, Reliable(method));
            onDataSent?.Invoke(target, data, true);
        }

        public void SendToServer(ByteData data, PChannel method = PChannel.ReliableOrdered)
        {
            if (_client == null) return;
            _client.SendToHost(data.segment, Reliable(method));
            onDataSent?.Invoke(ServerConn, data, false);
        }

        public void CloseConnection(Connection conn) => _host?.Kick(conn.connectionId);

        public void RaiseDataReceived(Connection conn, ByteData data, bool asServer) => onDataReceived?.Invoke(conn, data, asServer);
        public void RaiseDataSent(Connection conn, ByteData data, bool asServer) => onDataSent?.Invoke(conn, data, asServer);

        public void ReceiveMessages(float delta) { _host?.Pump(); _client?.Pump(); }
        public void SendMessages(float delta) { _host?.Flush(); _client?.Flush(); }

        public int GetMTU(Connection target, PChannel channel, bool asServer) => RelayBridge.Mtu(Reliable(channel));

        static bool Reliable(PChannel c) => c == PChannel.ReliableOrdered || c == PChannel.ReliableUnordered;

        static ConnectionState Map(BridgeState s) => s switch
        {
            BridgeState.Starting => ConnectionState.Connecting,
            BridgeState.Started => ConnectionState.Connected,
            BridgeState.Stopping => ConnectionState.Disconnecting,
            _ => ConnectionState.Disconnected
        };

        static DisconnectReason Map(BridgeLeave why, bool asServer) => why switch
        {
            BridgeLeave.Kicked => DisconnectReason.ServerRequest,
            BridgeLeave.Local => asServer ? DisconnectReason.ServerRequest : DisconnectReason.ClientRequest,
            BridgeLeave.Failed => DisconnectReason.ConnectionFailed,
            _ => DisconnectReason.Timeout
        };
    }
}
#endif
