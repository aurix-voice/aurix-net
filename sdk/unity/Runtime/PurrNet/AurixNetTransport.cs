// PurrNet transport over an Aurix Net relay room.
// Compiled only when the PurrNet package is present (AURIX_PURRNET is set by the asmdef version define).
// Topology: the PurrNet server (host) and every client join the same relay room; the room master is the
// PurrNet server. Server->client traffic is a targeted relay event, client->server is an event to the master.
// No host migration in this version: if the master changes, clients disconnect and PurrNet reconnects.
#if AURIX_PURRNET
using System;
using System.Collections.Generic;
using System.Text;
using System.Threading.Tasks;
using Aurix.Net.Managed;
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

        ManagedClient _host, _client;
        int _master = -1;
        static readonly Connection ServerConn = new Connection(0);

        // ---- GenericTransport ----
        protected override void StartClientInternal() => Connect(null, 0);
        protected override void StartServerInternal() => Listen(0);

        // ---- IListener (PurrNet server = room master) ----
        public async void Listen(ushort port)
        {
            if (listenerState != ConnectionState.Disconnected) return;
            SetListener(ConnectionState.Connecting);
            try
            {
                var g = await ResolveGrant();
                var c = await ManagedClient.ConnectAsync(httpUrl, g);
                await c.JoinOrCreateAsync(roomName, "relay", maxPlayers);
                _host = c;
                _master = (int)Json.Num(c.JoinedJson, "master");
                if (_master != c.Actor) throw new NetException("room already has a host (master actor " + _master + ")");
                c.OnControl += j => OnHostControl(j);
                c.OnEvent += f => { if (f.Code == eventCode) Raise(new Connection(f.From), f.Payload, true); };
                c.OnClosed += r => { SetListener(ConnectionState.Disconnected); };
                foreach (var a in Actors(c.JoinedJson)) if (a != c.Actor) AddConn(a);
                SetListener(ConnectionState.Connected);
            }
            catch (Exception e) { Debug.LogError("[AurixNet] listen failed: " + e.Message); SetListener(ConnectionState.Disconnected); }
        }

        public void StopListening()
        {
            if (_host == null) return;
            SetListener(ConnectionState.Disconnecting);
            foreach (var c in _connections.ToArray()) onDisconnected?.Invoke(c, DisconnectReason.ServerRequest, true);
            _connections.Clear();
            try { _host.Dispose(); } catch { }
            _host = null;
            SetListener(ConnectionState.Disconnected);
        }

        // ---- IConnectable (PurrNet client) ----
        public async void Connect(string ip, ushort port)
        {
            if (clientState != ConnectionState.Disconnected) return;
            SetClient(ConnectionState.Connecting);
            try
            {
                var g = await ResolveGrant();
                var c = await ManagedClient.ConnectAsync(httpUrl, g);
                await c.JoinAsync(roomName);
                _client = c;
                _master = (int)Json.Num(c.JoinedJson, "master");
                if (_master == c.Actor) throw new NetException("no host in room " + roomName);
                c.OnControl += j =>
                {
                    var t = Json.Str(j, "t");
                    if (t == "master_changed") Disconnect(DisconnectReason.Timeout);
                    else if (t == "left") Disconnect(Json.Str(j, "reason") == "kicked" ? DisconnectReason.ServerRequest : DisconnectReason.Timeout);
                };
                c.OnEvent += f => { if (f.Code == eventCode && f.From == _master) Raise(ServerConn, f.Payload, false); };
                c.OnClosed += r => { if (clientState != ConnectionState.Disconnected) { SetClient(ConnectionState.Disconnected); onDisconnected?.Invoke(ServerConn, DisconnectReason.Timeout, false); } };
                SetClient(ConnectionState.Connected);
                onConnected?.Invoke(ServerConn, false);
            }
            catch (Exception e)
            {
                Debug.LogError("[AurixNet] connect failed: " + e.Message);
                SetClient(ConnectionState.Disconnected);
                onDisconnected?.Invoke(ServerConn, DisconnectReason.ConnectionFailed, false);
            }
        }

        public void Disconnect() => Disconnect(DisconnectReason.ClientRequest);

        void Disconnect(DisconnectReason why)
        {
            if (_client == null) return;
            SetClient(ConnectionState.Disconnecting);
            try { _client.Dispose(); } catch { }
            _client = null;
            SetClient(ConnectionState.Disconnected);
            onDisconnected?.Invoke(ServerConn, why, false);
        }

        // ---- data ----
        public void SendToClient(Connection target, ByteData data, PChannel method = PChannel.ReliableOrdered)
        {
            if (_host == null || !target.isValid) return;
            _host.SendEvent(eventCode, Copy(data), Target.Actors, Reliable(method), new[] { (ushort)target.connectionId });
            onDataSent?.Invoke(target, data, true);
        }

        public void SendToServer(ByteData data, PChannel method = PChannel.ReliableOrdered)
        {
            if (_client == null) return;
            _client.SendEvent(eventCode, Copy(data), Target.Master, Reliable(method));
            onDataSent?.Invoke(ServerConn, data, false);
        }

        public void CloseConnection(Connection conn)
        {
            if (_host == null) return;
            _host.SendControlAsync("{\"t\":\"kick\",\"actor\":" + conn.connectionId + ",\"reason\":\"kicked\"}");
            RemoveConn(conn.connectionId, DisconnectReason.ServerRequest);
        }

        public void RaiseDataReceived(Connection conn, ByteData data, bool asServer) => onDataReceived?.Invoke(conn, data, asServer);
        public void RaiseDataSent(Connection conn, ByteData data, bool asServer) => onDataSent?.Invoke(conn, data, asServer);

        public void ReceiveMessages(float delta) { _host?.Poll(); _client?.Poll(); }
        public void SendMessages(float delta) { _host?.Flush(); _client?.Flush(); }

        public int GetMTU(Connection target, PChannel channel, bool asServer) =>
            Reliable(channel) ? 8192 : Packet.MaxUnfragmented - 8; // 8 = relay event header worst case

        // ---- helpers ----
        void OnHostControl(string j)
        {
            switch (Json.Str(j, "t"))
            {
                case "player_joined": { var p = Json.Obj(j, "player"); AddConn((int)Json.Num(p, "actor")); break; }
                case "player_left": RemoveConn((int)Json.Num(j, "actor"), DisconnectReason.ClientRequest); break;
                case "master_changed": if ((int)Json.Num(j, "actor") != _host.Actor) StopListening(); break;
            }
        }

        void AddConn(int actor)
        {
            var c = new Connection(actor);
            if (_connections.Contains(c)) return;
            _connections.Add(c);
            onConnected?.Invoke(c, true);
        }

        void RemoveConn(int actor, DisconnectReason why)
        {
            var c = new Connection(actor);
            if (_connections.Remove(c)) onDisconnected?.Invoke(c, why, true);
        }

        void Raise(Connection from, ArraySegment<byte> payload, bool asServer) =>
            onDataReceived?.Invoke(from, new ByteData(payload.Array, payload.Offset, payload.Count), asServer);

        async Task<string> ResolveGrant()
        {
            if (GrantProvider != null) return await GrantProvider();
            if (string.IsNullOrEmpty(grant)) throw new NetException("no grant: set AurixNetTransport.grant or GrantProvider");
            return grant;
        }

        static bool Reliable(PChannel c) => c == PChannel.ReliableOrdered || c == PChannel.ReliableUnordered;
        static byte[] Copy(ByteData d) { var b = new byte[d.length]; Array.Copy(d.data, d.offset, b, 0, d.length); return b; }

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

        void SetListener(ConnectionState s) { listenerState = s; onConnectionState?.Invoke(s, true); }
        void SetClient(ConnectionState s) { clientState = s; onConnectionState?.Invoke(s, false); }
    }
}
#endif
