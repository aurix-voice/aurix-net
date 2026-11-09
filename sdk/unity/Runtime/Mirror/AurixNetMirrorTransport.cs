// Mirror transport over an Aurix Net relay room (see Transports/RelayBridge.cs for the topology).
// Compiled only when Mirror is in the project (Mirror defines MIRROR).
#if MIRROR
using System;
using System.Threading.Tasks;
using Aurix.Net.Transports;
using Mirror;
using UnityEngine;

namespace Aurix.Net.MirrorTransport
{
    public sealed class AurixNetMirrorTransport : Transport
    {
        [Header("Aurix Net")]
        [Tooltip("Node control-plane URL, e.g. https://node.example.com")]
        public string httpUrl = "http://127.0.0.1:8090";
        [Tooltip("Grant issued by your backend (POST /v1/grants). Leave empty and set GrantProvider from code.")]
        public string grant = "";
        [Tooltip("Relay room name shared by host and clients. ClientConnect(address) overrides it when address is non-empty.")]
        public string roomName = "mirror";
        public int maxPlayers = 16;
        [Tooltip("Relay event code reserved for Mirror frames.")]
        public byte eventCode = 200;

        /// <summary>Async grant source; wins over the inspector field.</summary>
        public Func<Task<string>> GrantProvider;

        RelayBridge _server, _client;

        RelayBridge NewBridge(string room) => new RelayBridge(new RelayBridgeSettings
        {
            HttpUrl = httpUrl, Grant = grant, GrantProvider = GrantProvider, RoomName = room, MaxPlayers = maxPlayers, EventCode = eventCode
        });

        public override bool Available() => true;

        // ---- client ----
        public override bool ClientConnected() => _client != null && _client.State == BridgeState.Started;

        public override void ClientConnect(string address)
        {
            if (_client != null) return;
            var b = _client = NewBridge(string.IsNullOrEmpty(address) ? roomName : address);
            b.OnState += s => { if (s == BridgeState.Started) OnClientConnected?.Invoke(); };
            b.OnData += (from, d, _) => OnClientDataReceived?.Invoke(d, Channels.Reliable);
            b.OnDisconnected += why =>
            {
                if (_client == b) _client = null;
                if (why == BridgeLeave.Failed) OnClientError?.Invoke(TransportError.Refused, b.LastError);
                else if (why == BridgeLeave.HostGone || why == BridgeLeave.Lost) OnClientError?.Invoke(TransportError.ConnectionClosed, b.LastError);
                OnClientDisconnected?.Invoke();
            };
            b.StartClient();
        }

        public override void ClientSend(ArraySegment<byte> segment, int channelId = Channels.Reliable)
        {
            if (_client == null) return;
            _client.SendToHost(segment, channelId == Channels.Reliable);
            OnClientDataSent?.Invoke(segment, channelId);
        }

        public override void ClientDisconnect() { var b = _client; _client = null; b?.Stop(); }

        // ---- server ----
        public override Uri ServerUri() => new UriBuilder { Scheme = "aurix", Host = roomName }.Uri;
        public override bool ServerActive() => _server != null && _server.State == BridgeState.Started;

        public override void ServerStart()
        {
            if (_server != null) return;
            var b = _server = NewBridge(roomName);
            b.OnPeerJoined += a => { OnServerConnected?.Invoke(a); OnServerConnectedWithAddress?.Invoke(a, "actor:" + a); };
            b.OnPeerLeft += (a, why) => OnServerDisconnected?.Invoke(a);
            b.OnData += (from, d, _) => OnServerDataReceived?.Invoke(from, d, Channels.Reliable);
            b.OnDisconnected += why => { if (_server == b) _server = null; if (why != BridgeLeave.Local) Debug.LogWarning("[AurixNet] server stopped: " + b.LastError); };
            b.StartHost();
        }

        public override void ServerSend(int connectionId, ArraySegment<byte> segment, int channelId = Channels.Reliable)
        {
            if (_server == null) return;
            _server.SendTo(connectionId, segment, channelId == Channels.Reliable);
            OnServerDataSent?.Invoke(connectionId, segment, channelId);
        }

        public override void ServerDisconnect(int connectionId) => _server?.Kick(connectionId);
        public override string ServerGetClientAddress(int connectionId) => "actor:" + connectionId;
        public override void ServerStop() { var b = _server; _server = null; b?.Stop(); }

        // ---- common ----
        public override int GetMaxPacketSize(int channelId = Channels.Reliable) => RelayBridge.Mtu(channelId == Channels.Reliable);
        public override int GetBatchThreshold(int channelId = Channels.Reliable) => RelayBridge.Mtu(false);

        public override void ClientEarlyUpdate() => _client?.Pump();
        public override void ServerEarlyUpdate() => _server?.Pump();
        public override void ClientLateUpdate() => _client?.Flush();
        public override void ServerLateUpdate() => _server?.Flush();

        public override void Shutdown() { ClientDisconnect(); ServerStop(); }
    }
}
#endif
