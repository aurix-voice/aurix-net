// Pure C# Aurix Net client: no native library, works on every Unity platform including WebGL
// (via the WebSocket tunnel). Control channel = WebSocket JSON, data = WebSocket binary tunnel
// using the same packet format as QUIC. Call Poll() once per frame from the main thread.
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Net.WebSockets;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Newtonsoft.Json;
using Newtonsoft.Json.Linq;

namespace Aurix.Net.Managed
{
    public enum CacheOp : byte
    {
        None = 0,
        Add = 1,
        Remove = 2,
    }

    public sealed class ManagedClient : IDisposable
    {
        ClientWebSocket _ws;
        Link _link = new Link();

        // The link is worked from three places: the reader, the timer and the game's calls.
        readonly object _linkLock = new object();

        // Counts the data paths of this client: what an earlier one delivered is not the game's any more.
        int _epoch;
        bool _deadTold;

        // False between rooms: the link of a room the client has left must not go on resending to the node.
        bool _dataOpen;

        // One lost connection is reported once, whichever of the link and the control channel notices it first.
        bool _lost;
        bool _controlClosed;
        readonly ConcurrentQueue<object> _inbox = new ConcurrentQueue<object>();

        /// <summary>How many events wait for the game's Poll before the client stops reading.</summary>
        public const int EventBacklog = 4096;
        readonly SemaphoreSlim _slots = new SemaphoreSlim(EventBacklog, EventBacklog);
        internal int Waiting => _inbox.Count;
        readonly SemaphoreSlim _sendLock = new SemaphoreSlim(1, 1);
        CancellationTokenSource _cts;
        bool _dataReady;

        public int Actor { get; private set; } = -1;

        /// <summary>The player id of the grant this client signed in with: the id the node knows the player by in rooms, held seats and webhooks.</summary>
        public string PlayerId { get; private set; }

        /// <summary>The display name of the grant this client signed in with.</summary>
        public string PlayerName { get; private set; }

        /// <summary>URL given to ConnectAsync, the grant and the join that leads back into the last room: what RejoinAsync needs.</summary>
        public string EntryUrl { get; private set; }
        public string Grant { get; private set; }
        public string LastJoinJson { get; private set; }

        /// <summary>The node shut down and handed the room to another node: call RejoinAsync (NetRunner does).</summary>
        public event Action OnRehost;

        /// <summary>System notice from the operator or game backend: (text, level = "info" | "warn").</summary>
        public event Action<string, string> OnNotice;
        public string JoinedJson { get; private set; }
        public string RoomMode { get; private set; }

        /// <summary>Lockstep rooms: the seed every client and the node start the game from.</summary>
        public ulong? Seed { get; private set; }

        /// <summary>Sync rooms that answer tagged spawns: the first tag the room has not seen from this actor.</summary>
        public uint? NextSpawnTag { get; private set; }

        /// <summary>Sync rooms: the entity the room made for this client as its avatar, once the room has
        /// announced it. Kept from the join on, whether or not anything listens to the data.</summary>
        public ulong? Avatar { get; private set; }

        /// <summary>Events the room kept for late joiners, as received by the last join; also raised through OnEvent.</summary>
        public IReadOnlyList<EventFrame> Cached { get; private set; } = Array.Empty<EventFrame>();

        /// <summary>Raw control message (JSON text).</summary>
        public event Action<string> OnControl;

        /// <summary>Smoothed round trip of the control channel in milliseconds; 0 until the first sample.</summary>
        public float RttMs { get; private set; }
        readonly Stopwatch _clock = Stopwatch.StartNew();
        long _lastPingMs = -1000;

        // The probes in flight, by the time each carries: only their answers are the client's own.
        readonly ConcurrentDictionary<long, byte> _probes = new ConcurrentDictionary<long, byte>();
        const int ProbesKept = 16;

        /// <summary>Data-path message (channel, server tick, payload).</summary>
        public event Action<Channel, ushort, byte[]> OnData;

        /// <summary>Relay event decoded from the data path.</summary>
        public event Action<EventFrame> OnEvent;
        public event Action OnDataReady;
        public event Action<string> OnClosed;

        sealed class Closed
        {
            public string Reason;
            public bool Control = true;
        }

        sealed class Text
        {
            public string Json;

            // when it was read, on the client's clock
            public long At;
        }

        sealed class Got
        {
            public int Epoch;
            public List<Received> Items;
        }

        /// <summary>Opens the control channel and authenticates with a grant.</summary>
        public static async Task<ManagedClient> ConnectAsync(
            string httpUrl,
            string grant,
            int timeoutMs = 10000
        )
        {
            var client = new ManagedClient
            {
                _ws = new ClientWebSocket(),
                _cts = new CancellationTokenSource(),
                EntryUrl = httpUrl,
                Grant = grant,
            };
            var url =
                (httpUrl.StartsWith("https") ? "wss" : "ws")
                + httpUrl.Substring(httpUrl.IndexOf(':'))
                + "/ws";
            try
            {
                using (var timeout = new CancellationTokenSource(timeoutMs))
                {
                    await client._ws.ConnectAsync(new Uri(url), timeout.Token);
                }

                _ = client.ReadLoop();
                _ = client.TimerLoop();
                await client.SendControlAsync(
                    "{\"t\":\"auth\",\"grant\":"
                        + Json.Str(grant)
                        + ",\"supports\":[\"unsequenced\"]}"
                );
                var authed = await client.WaitForAsync("authed", timeoutMs);
                client.PlayerId = Json.Str(authed, "player_id");
                client.PlayerName = Json.Str(authed, "name");
                return client;
            }
            catch
            {
                client.Dispose();
                throw;
            }
        }

        /// <summary>connect + join, following one `room_elsewhere:&lt;url&gt;` redirect.</summary>
        public static async Task<ManagedClient> ConnectAndJoinAsync(
            string httpUrl,
            string grant,
            string joinJson,
            int timeoutMs = 10000
        )
        {
            try
            {
                return await JoinOrDisposeAsync(
                    await ConnectAsync(httpUrl, grant, timeoutMs),
                    joinJson,
                    timeoutMs
                );
            }
            catch (NetException error) when (error.Message.Contains("room_elsewhere:"))
            {
                var url = error
                    .Message.Substring(error.Message.IndexOf("room_elsewhere:") + 15)
                    .Trim();
                return await JoinOrDisposeAsync(
                    await ConnectAsync(url, grant, timeoutMs),
                    joinJson,
                    timeoutMs
                );
            }
        }

        static async Task<ManagedClient> JoinOrDisposeAsync(
            ManagedClient client,
            string joinJson,
            int timeoutMs
        )
        {
            try
            {
                await client.JoinRawAsync(joinJson, timeoutMs);
                return client;
            }
            catch
            {
                client.Dispose();
                throw;
            }
        }

        /// <summary>Joins the room of this name, creating it when there is none. The arguments
        /// after the name shape the room only when this call creates it: a hidden room
        /// (<paramref name="visible"/> false) is out of room listings and random joins, a closed one
        /// (<paramref name="open"/> false) lets nobody else in.</summary>
        public Task<int> JoinOrCreateAsync(
            string name,
            string mode = "relay",
            int maxPlayers = 8,
            string propsJson = null,
            string[] reserved = null,
            string[] lobbyProps = null,
            bool visible = true,
            bool open = true
        )
        {
            var sb = new StringBuilder("{\"t\":\"join_or_create\",\"name\":")
                .Append(Json.Str(name))
                .Append(",\"mode\":\"")
                .Append(mode)
                .Append("\",\"max_players\":")
                .Append(maxPlayers)
                .Append(",\"visible\":")
                .Append(visible ? "true" : "false")
                .Append(",\"open\":")
                .Append(open ? "true" : "false");
            if (!string.IsNullOrEmpty(propsJson))
            {
                sb.Append(",\"props\":").Append(propsJson);
            }

            if (reserved != null && reserved.Length > 0)
            {
                sb.Append(",\"reserved\":[");
                for (int i = 0; i < reserved.Length; i++)
                {
                    if (i > 0)
                    {
                        sb.Append(',');
                    }

                    sb.Append(Json.Str(reserved[i]));
                }
                sb.Append(']');
            }

            if (lobbyProps != null && lobbyProps.Length > 0)
            {
                sb.Append(",\"lobby_props\":[")
                    .Append(string.Join(",", lobbyProps.Select(Json.Str)))
                    .Append(']');
            }
            return JoinRawAsync(sb.Append('}').ToString());
        }

        /// <summary>Joins a random open room. <paramref name="filterJson"/> must equal room props exactly,
        /// <paramref name="expr"/> is a SQL-like condition on lobby props ("map = 'dust' AND skill <= 1500"),
        /// <paramref name="matchType"/> is "fill" (default), "serial" or "random".</summary>
        public Task<int> JoinRandomAsync(
            string filterJson = null,
            string expr = null,
            string matchType = null
        )
        {
            var sb = new StringBuilder("{\"t\":\"join_random\"");
            if (!string.IsNullOrEmpty(filterJson))
            {
                sb.Append(",\"filter\":").Append(filterJson);
            }

            if (!string.IsNullOrEmpty(expr))
            {
                sb.Append(",\"expr\":").Append(Json.Str(expr));
            }

            if (!string.IsNullOrEmpty(matchType))
            {
                sb.Append(",\"match_type\":").Append(Json.Str(matchType));
            }

            return JoinRawAsync(sb.Append('}').ToString());
        }

        public Task<int> JoinAsync(string name, string schemaHash = null, bool spectator = false)
        {
            var sb = new StringBuilder("{\"t\":\"join\",\"name\":").Append(Json.Str(name));
            if (schemaHash != null)
            {
                sb.Append(",\"schema_hash\":").Append(Json.Str(schemaHash));
            }

            if (spectator)
            {
                sb.Append(",\"spectator\":true");
            }

            return JoinRawAsync(sb.Append('}').ToString());
        }

        /// <summary>After a rehost: a fresh client in the same room with the same grant (entry URL, then fallbacks).</summary>
        public async Task<ManagedClient> RejoinAsync(params string[] fallbackUrls)
        {
            if (LastJoinJson == null)
            {
                throw new NetException("rejoin: never joined");
            }

            var msg = LastJoinJson;
            Dispose();
            Exception last = null;
            foreach (var url in new[] { EntryUrl }.Concat(fallbackUrls))
            {
                try
                {
                    return await ConnectAndJoinAsync(url, Grant, msg);
                }
                catch (Exception error)
                {
                    last = error;
                }
            }
            throw last;
        }

        /// <summary>Any join-like control message; opens the data path (ticket first) and waits for the server's ack.</summary>
        public async Task<int> JoinRawAsync(string json, int timeoutMs = 10000)
        {
            await SendControlAsync(json);
            var joined = await WaitForAsync("joined", timeoutMs);
            JoinedJson = joined;
            LastJoinJson = Json.WayBack(json, Json.Str(joined, "name"));
            Actor = (int)Json.Num(joined, "actor");
            RoomMode = Json.Str(joined, "mode");
            Seed = Json.ULong(joined, "seed");
            NextSpawnTag = (uint?)Json.ULong(joined, "next_spawn_tag");
            Avatar = null;
            Cached = Json.CachedEvents(joined);
            // Every join gets its own data path, and the node starts a fresh link on it.
            lock (_linkLock)
            {
                _link = new Link();
                _epoch++;
                _deadTold = false;
                _dataOpen = true;
            }
            _lost = false;
            _dataReady = false;
            var data = Json.Obj(joined, "data");
            if (Json.Bool(data, "ws_tunnel") == false)
            {
                throw new NetException("node has the WebSocket tunnel disabled");
            }

            var ticket = Convert.FromBase64String(Json.Str(data, "ticket"));
            await SendBinaryAsync(ticket);
            var deadline = DateTime.UtcNow.AddMilliseconds(timeoutMs);
            while (!_dataReady)
            {
                if (DateTime.UtcNow > deadline)
                {
                    throw new NetException("data path timeout");
                }

                await Task.Delay(5);
                Poll();
                ThrowIfControlClosed();
            }
            return Actor;
        }

        public Task SendControlAsync(string json) =>
            SendAsync(Encoding.UTF8.GetBytes(json), WebSocketMessageType.Text);

        Task SendBinaryAsync(byte[] bytes) => SendAsync(bytes, WebSocketMessageType.Binary);

        async Task SendAsync(byte[] bytes, WebSocketMessageType kind)
        {
            await _sendLock.WaitAsync().ConfigureAwait(false);
            try
            {
                if (_ws.State == WebSocketState.Open)
                {
                    await _ws.SendAsync(new ArraySegment<byte>(bytes), kind, true, _cts.Token)
                        .ConfigureAwait(false);
                }
            }
            finally
            {
                _sendLock.Release();
            }
        }

        public void SendData(Channel channel, byte[] payload)
        {
            if (!_dataOpen)
            {
                throw new NetException("disconnected");
            }

            if (channel == Channel.UnreliableUnsequenced && !_nodeTakesUnsequenced)
            {
                channel = Channel.Unreliable;
            }

            try
            {
                lock (_linkLock)
                {
                    _link.Send(channel, payload);
                }
            }
            catch (InvalidOperationException error)
            {
                TellDead();
                throw new NetException(error.Message);
            }
            if (channel == Channel.Reliable || channel == Channel.ReliableUnordered)
            {
                FlushLink();
            }
        }

        /// <param name="cache">Add keeps the event for late joiners (current cache slice, see CacheSliceAsync); Remove drops your earlier cached events with the same code.</param>
        public void SendEvent(
            byte code,
            byte[] payload,
            Target target = Target.Others,
            bool reliable = false,
            ushort[] actors = null,
            ushort? group = null,
            CacheOp cache = CacheOp.None
        ) =>
            SendEventOn(
                reliable || cache != CacheOp.None ? Channel.Reliable : Channel.Unreliable,
                code,
                payload,
                target,
                actors,
                group,
                cache
            );

        /// <summary><see cref="SendEvent"/> on a channel of the caller's choice: <see cref="Channel.Unreliable"/>
        /// for state, where a newer event replaces an older one, <see cref="Channel.UnreliableUnsequenced"/>
        /// for events that are all wanted. A node that does not know the unsequenced channel gets them on
        /// the sequenced one.</summary>
        public void SendEventOn(
            Channel channel,
            byte code,
            byte[] payload,
            Target target = Target.Others,
            ushort[] actors = null,
            ushort? group = null,
            CacheOp cache = CacheOp.None
        ) => SendData(channel, Event.Encode(code, payload, target, actors, group, cache));

        /// <summary>Relay event-cache slices: "increase" opens the next slice, "set" picks one, "purge" drops one slice, "purge_up_to" every slice up to index.</summary>
        public Task CacheSliceAsync(string op, ushort? index = null) =>
            SendControlAsync(
                "{\"t\":\"cache_slice\",\"op\":\""
                    + op
                    + "\""
                    + (index.HasValue ? ",\"index\":" + index.Value : "")
                    + "}"
            );

        /// <summary>Pushes queued unreliable messages; call at the end of the frame.</summary>
        public void Flush()
        {
            lock (_linkLock)
            {
                _link.FlushBatch();
                FlushLink();
            }
        }

        void FlushLink()
        {
            lock (_linkLock)
            {
                if (!_dataOpen)
                {
                    return;
                }

                while (_link.TryDequeueOutgoing(out var data))
                {
                    _ = SendBinaryAsync(data);
                }
            }
        }

        void TellDead()
        {
            lock (_linkLock)
            {
                if (_dataOpen && !_deadTold)
                {
                    _deadTold = true;
                    _inbox.Enqueue(new Closed { Reason = "link dead", Control = false });
                }
            }
        }

        /// <summary>Keeps the link going whether or not the game polls: the probe of the control channel, the resends and the batch.</summary>
        async Task TimerLoop()
        {
            try
            {
                while (_ws.State == WebSocketState.Open)
                {
                    await Task.Delay(10, _cts.Token).ConfigureAwait(false);
                    long now = _clock.ElapsedMilliseconds;
                    if (now - Interlocked.Read(ref _lastPingMs) >= 1000)
                    {
                        Interlocked.Exchange(ref _lastPingMs, now);
                        _probes[now] = 0;
                        if (_probes.Count > ProbesKept)
                        {
                            _probes.TryRemove(_probes.Keys.Min(), out _);
                        }
                        _ = SendControlAsync("{\"t\":\"ping\",\"ts\":" + now + "}");
                    }
                    lock (_linkLock)
                    {
                        if (_dataOpen && !_link.Poll())
                        {
                            TellDead();
                        }
                        FlushLink();
                    }
                }
            }
            catch (OperationCanceledException) { }
        }

        /// <summary>Dispatches pending messages on the calling thread. Call every frame.</summary>
        public int Poll(int max = 256)
        {
            int count = 0;
            while (count < max && _inbox.TryDequeue(out var message))
            {
                count++;
                switch (message)
                {
                    case Text text:
                        _slots.Release();
                        // The answer to the client's own probe is not the game's business.
                        if (
                            Json.Str(text.Json, "t") == "pong"
                            && _probes.TryRemove((long)Json.Num(text.Json, "ts"), out _)
                        )
                        {
                            float sample = Math.Max(0f, text.At - (long)Json.Num(text.Json, "ts"));
                            RttMs = RttMs > 0f ? 0.875f * RttMs + 0.125f * sample : sample;
                            break;
                        }
                        if (
                            Json.Str(text.Json, "t") == "left"
                            && Json.Str(text.Json, "reason") != "left"
                            && JoinedJson != null
                            && Json.Str(text.Json, "room") == Json.Str(JoinedJson, "room")
                        )
                        {
                            ForgetRoom();
                        }
                        if (Json.Str(text.Json, "t") == "hello")
                        {
                            _nodeTakesUnsequenced = Json.Lists(
                                text.Json,
                                "supports",
                                "unsequenced"
                            );
                        }

                        OnControl?.Invoke(text.Json);
                        if (
                            Json.Str(text.Json, "t") == "error"
                            && Json.Str(text.Json, "code") == "transport"
                        )
                        {
                            // the node gave the data path up and will take nothing more over it
                            Lose("link dead");
                        }

                        if (
                            Json.Str(text.Json, "t") == "left"
                            && Json.Str(text.Json, "reason") == "rehost"
                        )
                        {
                            OnRehost?.Invoke();
                        }

                        if (Json.Str(text.Json, "t") == "notice")
                        {
                            OnNotice?.Invoke(
                                Json.Str(text.Json, "text"),
                                Json.Str(text.Json, "level")
                            );
                        }

                        break;
                    case Got got:
                        _slots.Release(got.Items.Count);
                        if (got.Epoch != _epoch)
                        {
                            break;
                        }

                        foreach (var received in got.Items)
                        {
                            if (received.Payload.Length == 0)
                            {
                                if (!_dataReady)
                                {
                                    _dataReady = true;
                                    OnDataReady?.Invoke();
                                    foreach (var frame in Cached)
                                    {
                                        OnEvent?.Invoke(frame);
                                    }
                                }
                                continue;
                            }
                            if (
                                SyncWorld.ReadSpawnAnswer(received.Payload) is SpawnAnswer answer
                                && answer.Tag == 0
                                && answer.Status == SpawnStatus.Created
                            )
                            {
                                Avatar = answer.Entity;
                            }
                            OnData?.Invoke(received.Channel, received.Tick, received.Payload);
                            byte kind = (byte)(received.Payload[0] & 0xf0);
                            if (RoomMode == "relay" || (kind != 0x10 && kind != 0x20))
                            {
                                if (Event.TryDecode(received.Payload, out var frame))
                                {
                                    frame.Reliable =
                                        received.Channel == Channel.Reliable
                                        || received.Channel == Channel.ReliableUnordered;
                                    OnEvent?.Invoke(frame);
                                }
                            }
                        }
                        break;
                    case Closed closed:
                        _controlClosed |= closed.Control;
                        Lose(closed.Reason);
                        break;
                }
            }
            return count;
        }

        void CloseLink()
        {
            lock (_linkLock)
            {
                _dataOpen = false;
                _link = new Link();
                _epoch++;
            }
        }

        /// <summary>The data path is gone: the game is told once, and sends fail from here on.</summary>
        void Lose(string reason)
        {
            if (_lost)
            {
                return;
            }

            _lost = true;
            CloseLink();
            OnClosed?.Invoke(reason);
        }

        /// <summary>The client is out of its room: by LeaveAsync, or the node took it out (kick, room closed, rehost). Its data path closes with what was queued for it.</summary>
        void ForgetRoom()
        {
            CloseLink();
            Actor = -1;
            JoinedJson = null;
            RoomMode = null;
            Seed = null;
            NextSpawnTag = null;
            Avatar = null;
        }

        public Task LeaveAsync()
        {
            ForgetRoom();
            return SendControlAsync("{\"t\":\"leave\"}");
        }

        /// <summary>Project-wide live rooms/players grouped by the room prop "lobby" (JSON: {rooms, players, lobbies:[{lobby, rooms, players}]}); one per second.</summary>
        public async Task<string> StatsAsync(int timeoutMs = 10000)
        {
            var id = NextRequestId();
            await SendControlAsync("{\"t\":\"stats\",\"id\":" + id + "}");
            return await WaitForAsync("stats", timeoutMs, id);
        }

        /// <summary>Presence of up to 64 players across the fleet (JSON: {friends:[{player, online, room?:{id,name,mode,node}}]}); one per second.</summary>
        public async Task<string> FindFriendsAsync(string[] players, int timeoutMs = 10000)
        {
            var ids = new System.Text.StringBuilder();
            foreach (var player in players ?? Array.Empty<string>())
            {
                if (ids.Length > 0)
                {
                    ids.Append(',');
                }

                ids.Append(Json.Str(player));
            }
            var id = NextRequestId();
            await SendControlAsync(
                "{\"t\":\"find_friends\",\"players\":[" + ids + "],\"id\":" + id + "}"
            );
            return await WaitForAsync("friends", timeoutMs, id);
        }

        /// <summary>One-shot room list (JSON {rooms:[{id,name,mode,players,max_players,open,props}]}), optionally filtered by a SQL-like expr on lobby props; one per second.</summary>
        public async Task<string> RoomsAsync(
            string expr = null,
            int limit = 100,
            int timeoutMs = 10000
        )
        {
            var id = NextRequestId();
            await SendControlAsync(
                "{\"t\":\"get_rooms\",\"expr\":"
                    + (expr == null ? "null" : Json.Str(expr))
                    + ",\"limit\":"
                    + limit
                    + ",\"id\":"
                    + id
                    + "}"
            );
            return await WaitForAsync("rooms", timeoutMs, id);
        }

        int _requestSeq;
        bool _nodeTakesUnsequenced;

        // `stats`, `find_friends`, `get_rooms` and `rpc` share the ids: an `error` repeats only the number.
        uint NextRequestId() => (uint)System.Threading.Interlocked.Increment(ref _requestSeq);

        /// <summary>Call your backend through the node (WebRPC): the project's webhook subscribed to "rpc" is POSTed {name, player_id, actor?, room?, body}; returns the raw rpc_result JSON {id, ok, status?, body, error?}. bodyJson is a JSON literal (≤ 8 KiB); 5 calls/s.</summary>
        public async Task<string> RpcAsync(
            string name,
            string bodyJson = "null",
            int timeoutMs = 10000
        )
        {
            var id = NextRequestId();
            await SendControlAsync(
                "{\"t\":\"rpc\",\"id\":"
                    + id
                    + ",\"name\":"
                    + Json.Str(name)
                    + ",\"body\":"
                    + (string.IsNullOrEmpty(bodyJson) ? "null" : bodyJson)
                    + "}"
            );
            return await WaitForAsync("rpc_result", timeoutMs, id);
        }

        /// <summary>Relay interest groups: events raised with a group reach only its members.</summary>
        public Task SetGroupsAsync(ushort[] join = null, ushort[] leave = null) =>
            SendControlAsync(
                "{\"t\":\"set_groups\",\"join\":["
                    + string.Join(",", join ?? Array.Empty<ushort>())
                    + "],\"leave\":["
                    + string.Join(",", leave ?? Array.Empty<ushort>())
                    + "]}"
            );

        /// <summary>Sync rooms: steer area-of-interest: extra viewpoints (camera, scope target) and entities forced into view (team mates, objectives). Entity ids as in deltas.</summary>
        public Task SetViewAsync(ulong[] viewpoints = null, ulong[] interest = null) =>
            SendControlAsync(
                "{\"t\":\"set_view\",\"viewpoints\":["
                    + string.Join(",", viewpoints ?? Array.Empty<ulong>())
                    + "],\"interest\":["
                    + string.Join(",", interest ?? Array.Empty<ulong>())
                    + "]}"
            );

        async Task ReadLoop()
        {
            var buf = new byte[64 * 1024];
            var acc = new List<byte>();
            try
            {
                while (_ws.State == WebSocketState.Open)
                {
                    var result = await _ws.ReceiveAsync(new ArraySegment<byte>(buf), _cts.Token)
                        .ConfigureAwait(false);
                    if (result.MessageType == WebSocketMessageType.Close)
                    {
                        break;
                    }

                    for (int i = 0; i < result.Count; i++)
                    {
                        acc.Add(buf[i]);
                    }

                    if (!result.EndOfMessage)
                    {
                        continue;
                    }

                    var msg = acc.ToArray();
                    acc.Clear();
                    if (result.MessageType == WebSocketMessageType.Text)
                    {
                        await _slots.WaitAsync(_cts.Token).ConfigureAwait(false);
                        _inbox.Enqueue(
                            new Text
                            {
                                Json = Encoding.UTF8.GetString(msg),
                                At = _clock.ElapsedMilliseconds,
                            }
                        );
                        continue;
                    }

                    Got got = null;
                    lock (_linkLock)
                    {
                        if (_dataOpen)
                        {
                            var items = _link.Receive(msg);
                            if (items.Count > 0)
                            {
                                got = new Got { Epoch = _epoch, Items = items };
                            }
                            FlushLink();
                        }
                    }
                    if (got != null)
                    {
                        for (int i = 0; i < got.Items.Count; i++)
                        {
                            await _slots.WaitAsync(_cts.Token).ConfigureAwait(false);
                        }
                        _inbox.Enqueue(got);
                    }
                }
                _inbox.Enqueue(new Closed { Reason = "control channel closed" });
            }
            catch (Exception error)
            {
                if (!_cts.IsCancellationRequested)
                {
                    _inbox.Enqueue(new Closed { Reason = error.Message });
                }
            }
        }

        /// <summary>Waits for a control message of type `type`, polling the inbox (other messages are dispatched
        /// normally). With `id`, the request's own: an answer or `error` that repeats another id is not this
        /// request's. One without an id is taken, as a node that repeats no ids sends it.</summary>
        async Task<string> WaitForAsync(string type, int timeoutMs, uint? id = null)
        {
            var deadline = DateTime.UtcNow.AddMilliseconds(timeoutMs);
            string got = null,
                err = null;
            Action<string> handler = json =>
            {
                var ty = Json.Str(json, "t");
                var repeated = Json.ULong(json, "id");
                if (ty == type && (id == null || repeated == null || repeated == id))
                {
                    got = json;
                }
                else if (ty == "error" && (repeated == null || repeated == id))
                {
                    err = Json.Str(json, "code") + ": " + Json.Str(json, "message");
                }
            };
            OnControl += handler;
            try
            {
                while (got == null && err == null)
                {
                    if (DateTime.UtcNow > deadline)
                    {
                        throw new NetException("timeout waiting for " + type);
                    }

                    Poll();
                    if (got == null && err == null)
                    {
                        ThrowIfControlClosed();
                    }

                    await Task.Delay(2);
                }
            }
            finally
            {
                OnControl -= handler;
            }
            if (err != null)
            {
                throw new NetException(err);
            }

            return got;
        }

        /// <summary>No answer can come any more: a call waiting for one fails now, not at its timeout.</summary>
        void ThrowIfControlClosed()
        {
            if (_controlClosed)
            {
                throw new NetException("control channel closed");
            }
        }

        public void Dispose()
        {
            try
            {
                _cts?.Cancel();
                _ws?.Dispose();
            }
            catch { }
        }
    }

    /// <summary>Relay event frame codec (u8 code, u8 flags, u16 from, [u16 group], [u16 n, n*u16], payload).</summary>
    public static class Event
    {
        public static byte[] Encode(
            byte code,
            byte[] payload,
            Target target,
            ushort[] actors = null,
            ushort? group = null,
            CacheOp cache = CacheOp.None
        )
        {
            int count = target == Target.Actors ? (actors?.Length ?? 0) : 0;
            var bytes = new byte[
                4
                    + (group.HasValue ? 2 : 0)
                    + (target == Target.Actors ? 2 + count * 2 : 0)
                    + (payload?.Length ?? 0)
            ];
            int offset = 0;
            bytes[offset++] = code;
            bytes[offset++] = (byte)(
                (int)target
                | (group.HasValue ? 0x10 : 0)
                | (
                    cache == CacheOp.Add ? 0x04
                    : cache == CacheOp.Remove ? 0x08
                    : 0
                )
            );
            bytes[offset++] = 0;
            bytes[offset++] = 0;
            if (group.HasValue)
            {
                bytes[offset++] = (byte)(group.Value >> 8);
                bytes[offset++] = (byte)group.Value;
            }
            if (target == Target.Actors)
            {
                bytes[offset++] = (byte)(count >> 8);
                bytes[offset++] = (byte)count;
                for (int i = 0; i < count; i++)
                {
                    bytes[offset++] = (byte)(actors[i] >> 8);
                    bytes[offset++] = (byte)actors[i];
                }
            }
            if (payload != null)
            {
                Array.Copy(payload, 0, bytes, offset, payload.Length);
            }

            return bytes;
        }

        public static bool TryDecode(byte[] buf, out EventFrame frame) =>
            EventFrame.TryDecode(buf, out frame);
    }

    /// <summary>Minimal JSON field access for the handful of control fields the client needs.</summary>
    internal static class Json
    {
        public static string Str(string text) => JsonConvert.ToString(text);

        /// <summary>The join that brings a client back into the room `asked` led to: the room by its name, as
        /// the player that entered it. A random join or a join_or_create asked again could lead elsewhere.</summary>
        public static string WayBack(string asked, string name)
        {
            var was = JObject.Parse(asked);
            var again = new JObject { ["t"] = "join", ["name"] = name };
            var kept =
                (string)was["t"] == "join"
                    ? new[] { "player_props", "schema_hash", "spectator" }
                    : new[] { "player_props" };
            foreach (var key in kept)
            {
                if (was[key] != null)
                {
                    again[key] = was[key];
                }
            }
            return again.ToString(Formatting.None);
        }

        public static JToken Parse(string json)
        {
            if (string.IsNullOrEmpty(json))
            {
                return null;
            }

            try
            {
                using var reader = new JsonTextReader(new StringReader(json))
                {
                    DateParseHandling = DateParseHandling.None,
                };
                return JToken.ReadFrom(reader);
            }
            catch (JsonException)
            {
                return null;
            }
        }

        static JToken Field(string json, string key) => (Parse(json) as JObject)?[key];

        public static string Str(string json, string key) =>
            Field(json, key) is JValue { Type: JTokenType.String } value ? (string)value : null;

        public static bool Lists(string json, string key, string item) =>
            Field(json, key) is JArray list
            && list.Any(entry =>
                entry is JValue { Type: JTokenType.String } value && (string)value == item
            );

        public static double Num(string json, string key) =>
            Field(json, key) is JValue { Type: JTokenType.Integer or JTokenType.Float } value
                ? (double)value
                : double.NaN;

        public static ulong? ULong(string json, string key) =>
            Field(json, key) is JValue { Type: JTokenType.Integer } value
                ? (ulong)value
                : (ulong?)null;

        public static bool? Bool(string json, string key) =>
            Field(json, key) is JValue { Type: JTokenType.Boolean } value
                ? (bool)value
                : (bool?)null;

        /// <summary>The `cached` field of a `joined` message: [code, from, base64 payload] per event.</summary>
        public static EventFrame[] CachedEvents(string joined) =>
            Field(joined, "cached") is JArray events
                ? events
                    .OfType<JArray>()
                    .Where(entry => entry.Count == 3)
                    .Select(entry => new EventFrame
                    {
                        Code = (byte)entry[0],
                        From = (ushort)entry[1],
                        Payload = new ArraySegment<byte>(
                            Convert.FromBase64String((string)entry[2])
                        ),
                        Reliable = true,
                        Cached = true,
                    })
                    .ToArray()
                : Array.Empty<EventFrame>();

        /// <summary>The object value for `key`, as JSON text.</summary>
        public static string Obj(string json, string key) =>
            (Field(json, key) as JObject)?.ToString(Formatting.None);
    }
}

namespace Aurix.Net.Managed
{
    /// <summary>Small JSON helpers exposed for samples/tests (field access on control messages).</summary>
    public static class ManagedClientTestHooks
    {
        public static string Str(string json, string key) => Json.Str(json, key);

        public static double Num(string json, string key) => Json.Num(json, key);

        public static ulong? ULong(string json, string key) => Json.ULong(json, key);

        /// <summary>Messages read from the node that wait for the game's Poll.</summary>
        public static int Waiting(ManagedClient client) => client.Waiting;
    }
}
