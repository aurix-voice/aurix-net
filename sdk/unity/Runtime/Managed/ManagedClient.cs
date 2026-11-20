// Pure C# Aurix Net client: no native library, works on every Unity platform including WebGL
// (via the WebSocket tunnel). Control channel = WebSocket JSON, data = WebSocket binary tunnel
// using the same packet format as QUIC. Call Poll() once per frame from the main thread.
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
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

    public sealed class ManagedClient : IDisposable, ISocketOwner
    {
        ISocket _socket;
        Link _link = new Link();

        /// <summary>The mark of an event among the messages of the link.</summary>
        const int EventMark = 1;

        // The link is worked from three places: the reader, the timer and the game's calls.
        readonly object _linkLock = new object();

        // Counts the data paths of this client: what an earlier one delivered is not the game's any more.
        int _epoch;

        /// <summary>Another value in every room the client enters and once it is out of it.</summary>
        internal int Stay => Volatile.Read(ref _epoch);
        bool _deadTold;

        // False between rooms: the link of a room the client has left must not go on resending to the node.
        bool _dataOpen;

        // True while LeaveAsync waits for the node's acknowledgements: the client takes no new sends.
        bool _leaving;

        // One lost connection is reported once, whichever of the link and the control channel notices it first.
        bool _lost;

        // Set once the control channel is closed: what the calls made after it fail with.
        string _controlClosed;
        readonly ConcurrentQueue<object> _inbox = new ConcurrentQueue<object>();

        /// <summary>The largest control message the node reads, in bytes of its JSON: a call that
        /// would send a larger one sends nothing and throws.</summary>
        public const int MaxControlMessage = 69632;

        /// <summary>The same for the sign-in, the message with the grant.</summary>
        public const int MaxControlMessageBeforeAuth = 8192;

        internal const string ControlClosed = "control channel closed";

        /// <summary>How many events wait for the game's Poll before the client stops reading.</summary>
        public const int EventBacklog = 4096;
        internal int Waiting => _inbox.Count;
        bool _dataReady;
        Got _unfinished;
        Queue<EventFrame> _history = new Queue<EventFrame>();

        // The reason the node took the client out of a room its join had not entered yet.
        string _turnedOut;

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

        /// <summary>The reliable events the node had not acknowledged when the room was lost, in the
        /// order they were sent. Comes before OnClosed and before the `left` of a room the node took
        /// the client out of or LeaveAsync left without their acknowledgement. The room may have got
        /// any of them.</summary>
        public event Action<IReadOnlyList<UndeliveredEvent>> OnUndelivered;

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

            /// <summary>The first message the game has not been given yet.</summary>
            public int Next;
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
#if UNITY_WEBGL && !UNITY_EDITOR
                _socket = new BrowserSocket(EventBacklog),
#else
                _socket = new SystemSocket(EventBacklog),
#endif
                EntryUrl = httpUrl,
                Grant = grant,
            };
            var url =
                (httpUrl.StartsWith("https") ? "wss" : "ws")
                + httpUrl.Substring(httpUrl.IndexOf(':'))
                + "/ws";
            try
            {
                await client._socket.OpenAsync(url, timeoutMs, client);
                await client.Send(
                    new JObject
                    {
                        ["t"] = "auth",
                        ["grant"] = grant,
                        ["supports"] = new JArray("unsequenced"),
                    },
                    MaxControlMessageBeforeAuth
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
        public static Task<ManagedClient> ConnectAndJoinAsync(
            string httpUrl,
            string grant,
            string joinJson,
            int timeoutMs = 10000
        ) => ConnectAndJoin(httpUrl, grant, Joining(joinJson, nameof(joinJson)), timeoutMs);

        static async Task<ManagedClient> ConnectAndJoin(
            string httpUrl,
            string grant,
            JObject join,
            int timeoutMs
        )
        {
            try
            {
                return await JoinOrDisposeAsync(
                    await ConnectAsync(httpUrl, grant, timeoutMs),
                    join,
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
                    join,
                    timeoutMs
                );
            }
        }

        static async Task<ManagedClient> JoinOrDisposeAsync(
            ManagedClient client,
            JObject join,
            int timeoutMs
        )
        {
            try
            {
                await client.Join((JObject)join.DeepClone(), timeoutMs);
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
            var message = new JObject
            {
                ["t"] = "join_or_create",
                ["name"] = name,
                ["mode"] = mode,
                ["max_players"] = maxPlayers,
                ["visible"] = visible,
                ["open"] = open,
            };
            if (!string.IsNullOrEmpty(propsJson))
            {
                message["props"] = Json.Object(propsJson, nameof(propsJson));
            }

            if (reserved != null && reserved.Length > 0)
            {
                message["reserved"] = new JArray(reserved);
            }

            if (lobbyProps != null && lobbyProps.Length > 0)
            {
                message["lobby_props"] = new JArray(lobbyProps);
            }

            return Join(message);
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
            var message = new JObject { ["t"] = "join_random" };
            if (!string.IsNullOrEmpty(filterJson))
            {
                message["filter"] = Json.Object(filterJson, nameof(filterJson));
            }

            if (!string.IsNullOrEmpty(expr))
            {
                message["expr"] = expr;
            }

            if (!string.IsNullOrEmpty(matchType))
            {
                message["match_type"] = matchType;
            }

            return Join(message);
        }

        public Task<int> JoinAsync(string name, string schemaHash = null, bool spectator = false)
        {
            var message = new JObject { ["t"] = "join", ["name"] = name };
            if (schemaHash != null)
            {
                message["schema_hash"] = schemaHash;
            }

            if (spectator)
            {
                message["spectator"] = true;
            }

            return Join(message);
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
        public Task<int> JoinRawAsync(string json, int timeoutMs = 10000) =>
            Join(Joining(json, nameof(json)), timeoutMs);

        static readonly string[] Joins = { "create", "join", "join_or_create", "join_random" };

        /// <summary>The message a game hands over as the text of a join: the node answers these with
        /// `joined`, and nothing else it is sent.</summary>
        static JObject Joining(string json, string what)
        {
            var message = Json.Object(json, what);
            if (
                !(message["t"] is JValue { Type: JTokenType.String } kind)
                || !Joins.Contains((string)kind)
            )
            {
                throw new ArgumentException("not a " + string.Join(", ", Joins), what);
            }

            return message;
        }

        async Task<int> Join(JObject message, int timeoutMs = 10000)
        {
            // A node that repeats the id of a join answers this one and no other with it.
            uint? id = _nodeRepeatsJoinIds ? NextRequestId() : (uint?)null;
            if (id != null)
            {
                message["id"] = id;
            }

            await Send(message);
            _turnedOut = null;
            // A `left` of this room that follows is the end of this join.
            var joined = await AnswerAsync(
                "joined",
                timeoutMs,
                id,
                answer =>
                {
                    JoinedJson = answer;
                    _dataReady = false;
                }
            );
            if (joined == null)
            {
                // The node may have let the client in, or may yet: the client is in no room after this.
                await LeaveAsync();
                throw new NetException("timeout waiting for joined");
            }

            ThrowIfTurnedOut();
            LastJoinJson = Json.WayBack(
                message.ToString(Formatting.None),
                Json.Str(joined, "name")
            );
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

                await _socket.Delay(5);
                Poll();
                ThrowIfTurnedOut();
                ThrowIfControlClosed();
            }
            return Actor;
        }

        void ThrowIfTurnedOut()
        {
            if (_turnedOut != null)
            {
                throw new NetException("left: " + _turnedOut);
            }
        }

        /// <summary>Any control message as the text of its JSON object, e.g.
        /// {"t":"set_room_props","props":{...}}. Text that is not one JSON object is refused and
        /// not sent.</summary>
        public Task SendControlAsync(string json) => Send(Json.Object(json, nameof(json)));

        Task Send(JObject message, int most = MaxControlMessage)
        {
            var bytes = Encoding.UTF8.GetBytes(message.ToString(Formatting.None));
            if (bytes.Length > most)
            {
                throw new ArgumentOutOfRangeException(
                    nameof(message),
                    "control message of " + bytes.Length + " bytes: at most " + most
                );
            }

            return _socket.SendAsync(bytes, true);
        }

        Task SendBinaryAsync(byte[] bytes) => _socket.SendAsync(bytes, false);

        public void SendData(Channel channel, byte[] payload) => Queue(channel, payload, 0);

        void Queue(Channel channel, byte[] payload, int mark)
        {
            if (!_dataOpen || _leaving)
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
                    _link.SendMarked(channel, payload, mark);
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
        ) => Queue(channel, Event.Encode(code, payload, target, actors, group, cache), EventMark);

        /// <summary>Relay event-cache slices: "increase" opens the next slice, "set" picks one, "purge" drops one slice, "purge_up_to" every slice up to index.</summary>
        public Task CacheSliceAsync(string op, ushort? index = null)
        {
            var message = new JObject { ["t"] = "cache_slice", ["op"] = op };
            if (index.HasValue)
            {
                message["index"] = index.Value;
            }

            return Send(message);
        }

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
        void ISocketOwner.Tick()
        {
            long now = _clock.ElapsedMilliseconds;
            if (now - Interlocked.Read(ref _lastPingMs) >= 1000)
            {
                Interlocked.Exchange(ref _lastPingMs, now);
                _probes[now] = 0;
                if (_probes.Count > ProbesKept)
                {
                    _probes.TryRemove(_probes.Keys.Min(), out _);
                }
                _ = Send(new JObject { ["t"] = "ping", ["ts"] = now });
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

        /// <summary>Dispatches pending messages on the calling thread. Call every frame.</summary>
        public int Poll(int max = 256)
        {
            int count = 0;
            while (count < max)
            {
                object message = _unfinished;
                if (message == null && !_inbox.TryDequeue(out message))
                {
                    break;
                }

                count++;
                switch (message)
                {
                    case Text text:
                        _socket.Release(1);
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
                            bool entered = _dataReady;
                            var events = UnacknowledgedEvents();
                            ForgetRoom();
                            if (!entered)
                            {
                                // The join fails with the reason, and the game hears of no room it was never in.
                                _turnedOut = Json.Str(text.Json, "reason");
                                break;
                            }

                            HandBack(events);
                        }
                        if (Json.Str(text.Json, "t") == "hello")
                        {
                            _nodeTakesUnsequenced = Json.Lists(
                                text.Json,
                                "supports",
                                "unsequenced"
                            );
                            _nodeRepeatsJoinIds = Json.Lists(text.Json, "supports", "join_id");
                        }

                        try
                        {
                            OnControl?.Invoke(text.Json);
                        }
                        finally
                        {
                            if (
                                Json.Str(text.Json, "t") == "error"
                                && Json.Str(text.Json, "code") == "transport"
                            )
                            {
                                // the node gave the data path up and will take nothing more over it
                                Lose("link dead");
                            }
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
                        // A handler that throws leaves Poll in the middle of the datagram: it is
                        // kept, and the next Poll goes on with the message after that one.
                        if (_unfinished != got)
                        {
                            _socket.Release(got.Items.Count);
                            _unfinished = got;
                        }
                        // what the room sent is not the game's once it is out of the room
                        while (got.Next < got.Items.Count && got.Epoch == _epoch)
                        {
                            var received = got.Items[got.Next];
                            if (received.Payload.Length == 0)
                            {
                                if (!_dataReady)
                                {
                                    _dataReady = true;
                                    _history = new Queue<EventFrame>(Cached);
                                    OnDataReady?.Invoke();
                                }
                                while (_history.Count > 0 && got.Epoch == _epoch)
                                {
                                    OnEvent?.Invoke(_history.Dequeue());
                                }
                                got.Next++;
                                continue;
                            }
                            got.Next++;
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
                        _unfinished = null;
                        break;
                    case Closed closed:
                        if (closed.Control && _controlClosed == null)
                        {
                            // a socket that failed says so in its own words; a close says the node's
                            _controlClosed = closed.Reason.StartsWith(ControlClosed)
                                ? closed.Reason
                                : ControlClosed;
                        }

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
            var events = UnacknowledgedEvents();
            CloseLink();
            HandBack(events);
            OnClosed?.Invoke(reason);
        }

        List<UndeliveredEvent> UnacknowledgedEvents()
        {
            lock (_linkLock)
            {
                return _link
                    .Unacknowledged()
                    .Where(message => message.Mark == EventMark)
                    .Select(message => Event.DecodeSent(message.Msg))
                    .ToList();
            }
        }

        void HandBack(List<UndeliveredEvent> events)
        {
            if (events.Count > 0)
            {
                OnUndelivered?.Invoke(events);
            }
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

        /// <summary>Leaves the room once the node has acknowledged what the game sent it reliably, or after
        /// Link.LeaveWait: the events it has not acknowledged by then come in OnUndelivered before the `left`.
        /// Until then the client stays in the room, takes no new sends and dispatches what comes as Poll does.</summary>
        public async Task LeaveAsync()
        {
            _leaving = true;
            try
            {
                long end = _clock.ElapsedMilliseconds + (long)Link.LeaveWait;
                while (AwaitsAcknowledgement() && _clock.ElapsedMilliseconds < end)
                {
                    Poll();
                    await _socket.Delay(2);
                }
            }
            finally
            {
                _leaving = false;
            }
            var events = UnacknowledgedEvents();
            ForgetRoom();
            HandBack(events);
            var leave = new JObject { ["t"] = "leave" };
            if (_nodeRepeatsJoinIds)
            {
                leave["id"] = NextRequestId();
            }

            await Send(leave);
        }

        bool AwaitsAcknowledgement()
        {
            lock (_linkLock)
            {
                return _dataOpen && _link.Unacknowledged().Count > 0;
            }
        }

        /// <summary>Project-wide live rooms/players grouped by the room prop "lobby" (JSON: {rooms, players, lobbies:[{lobby, rooms, players}]}); one per second.</summary>
        public async Task<string> StatsAsync(int timeoutMs = 10000)
        {
            var id = NextRequestId();
            await Send(new JObject { ["t"] = "stats", ["id"] = id });
            return await WaitForAsync("stats", timeoutMs, id);
        }

        /// <summary>Presence of up to 64 players across the fleet (JSON: {friends:[{player, online, room?:{id,name,mode,node}}]}); one per second.</summary>
        public async Task<string> FindFriendsAsync(string[] players, int timeoutMs = 10000)
        {
            var id = NextRequestId();
            await Send(
                new JObject
                {
                    ["t"] = "find_friends",
                    ["players"] = new JArray(players ?? Array.Empty<string>()),
                    ["id"] = id,
                }
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
            await Send(
                new JObject
                {
                    ["t"] = "get_rooms",
                    ["expr"] = expr,
                    ["limit"] = limit,
                    ["id"] = id,
                }
            );
            return await WaitForAsync("rooms", timeoutMs, id);
        }

        int _requestSeq;
        bool _nodeTakesUnsequenced;
        bool _nodeRepeatsJoinIds;

        // Every request with an id shares the ids: an `error` repeats only the number.
        uint NextRequestId() => (uint)System.Threading.Interlocked.Increment(ref _requestSeq);

        /// <summary>Call your backend through the node (WebRPC): the project's webhook subscribed to "rpc" is POSTed {name, player_id, actor?, room?, body}; returns the raw rpc_result JSON {id, ok, status?, body, error?}. bodyJson is a JSON literal (≤ 8 KiB); 5 calls/s.</summary>
        public async Task<string> RpcAsync(
            string name,
            string bodyJson = "null",
            int timeoutMs = 10000
        )
        {
            var body = string.IsNullOrEmpty(bodyJson)
                ? JValue.CreateNull()
                : Json.Value(bodyJson, nameof(bodyJson));
            var id = NextRequestId();
            await Send(
                new JObject
                {
                    ["t"] = "rpc",
                    ["id"] = id,
                    ["name"] = name,
                    ["body"] = body,
                }
            );
            return await WaitForAsync("rpc_result", timeoutMs, id);
        }

        /// <summary>Relay interest groups: events raised with a group reach only its members.</summary>
        public Task SetGroupsAsync(ushort[] join = null, ushort[] leave = null) =>
            Send(
                new JObject
                {
                    ["t"] = "set_groups",
                    ["join"] = new JArray(join ?? Array.Empty<ushort>()),
                    ["leave"] = new JArray(leave ?? Array.Empty<ushort>()),
                }
            );

        /// <summary>Sync rooms: steer area-of-interest: extra viewpoints (camera, scope target) and entities forced into view (team mates, objectives). Entity ids as in deltas.</summary>
        public Task SetViewAsync(ulong[] viewpoints = null, ulong[] interest = null) =>
            Send(
                new JObject
                {
                    ["t"] = "set_view",
                    ["viewpoints"] = new JArray(viewpoints ?? Array.Empty<ulong>()),
                    ["interest"] = new JArray(interest ?? Array.Empty<ulong>()),
                }
            );

        async Task ISocketOwner.Message(byte[] data, bool text)
        {
            if (text)
            {
                await _socket.Admit(1).ConfigureAwait(false);
                _inbox.Enqueue(
                    new Text
                    {
                        Json = Encoding.UTF8.GetString(data),
                        At = _clock.ElapsedMilliseconds,
                    }
                );
                return;
            }

            Got got = null;
            lock (_linkLock)
            {
                if (_dataOpen)
                {
                    var items = _link.Receive(data);
                    if (items.Count > 0)
                    {
                        got = new Got { Epoch = _epoch, Items = items };
                    }
                    FlushLink();
                }
            }
            if (got != null)
            {
                await _socket.Admit(got.Items.Count).ConfigureAwait(false);
                _inbox.Enqueue(got);
            }
        }

        void ISocketOwner.Closed(string reason) => _inbox.Enqueue(new Closed { Reason = reason });

        /// <summary>Waits for a control message of type `type`, polling the inbox (other messages are dispatched
        /// normally). With `id`, the request's own: an answer or `error` that repeats another id is not this
        /// request's. One without an id is taken, as a node that repeats no ids sends it.</summary>
        async Task<string> WaitForAsync(string type, int timeoutMs, uint? id = null) =>
            await AnswerAsync(type, timeoutMs, id)
            ?? throw new NetException("timeout waiting for " + type);

        /// <summary>`WaitForAsync` that returns null when the answer did not come in time.</summary>
        async Task<string> AnswerAsync(
            string type,
            int timeoutMs,
            uint? id,
            Action<string> taken = null
        )
        {
            var deadline = DateTime.UtcNow.AddMilliseconds(timeoutMs);
            string got = null,
                err = null;
            Action<string> handler = json =>
            {
                // the first answer is the answer: what a Poll hands out after it is not
                if (got != null || err != null)
                {
                    return;
                }

                var ty = Json.Str(json, "t");
                var repeated = Json.ULong(json, "id");
                if (ty == type && (id == null || repeated == null || repeated == id))
                {
                    got = json;
                    taken?.Invoke(json);
                }
                else if (ty == "error" && repeated == id)
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
                        return null;
                    }

                    Poll();
                    if (got == null && err == null)
                    {
                        ThrowIfControlClosed();
                    }

                    await _socket.Delay(2);
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
            if (_controlClosed != null)
            {
                throw new NetException(_controlClosed);
            }
        }

        public void Dispose()
        {
            try
            {
                _socket?.Dispose();
            }
            catch { }
        }
    }

    /// <summary>A reliable event the node did not acknowledge, with what SendEvent was given.</summary>
    public struct UndeliveredEvent
    {
        public byte Code;
        public byte[] Payload;
        public Target Target;
        public ushort[] Actors;
        public ushort? Group;
        public CacheOp Cache;
    }

    /// <summary>Relay event frame codec (u8 code, u8 flags, u16 from, [u16 group], [u16 n, n*u16], payload).</summary>
    public static class Event
    {
        /// <summary>Reads an event back from the bytes <see cref="Encode"/> made of it.</summary>
        public static UndeliveredEvent DecodeSent(byte[] bytes)
        {
            byte flags = bytes[1];
            var sent = new UndeliveredEvent
            {
                Code = bytes[0],
                Target = (Target)(flags & 3),
                Cache =
                    (flags & 0x04) != 0 ? CacheOp.Add
                    : (flags & 0x08) != 0 ? CacheOp.Remove
                    : CacheOp.None,
            };
            int offset = 4;
            if ((flags & 0x10) != 0)
            {
                sent.Group = (ushort)(bytes[offset] << 8 | bytes[offset + 1]);
                offset += 2;
            }
            if (sent.Target == Target.Actors)
            {
                sent.Actors = new ushort[bytes[offset] << 8 | bytes[offset + 1]];
                offset += 2;
                for (int i = 0; i < sent.Actors.Length; i++, offset += 2)
                {
                    sent.Actors[i] = (ushort)(bytes[offset] << 8 | bytes[offset + 1]);
                }
            }
            sent.Payload = bytes.Skip(offset).ToArray();
            return sent;
        }

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

        // The node refuses a field named twice; read loosely, the later one would replace the earlier.
        static readonly JsonLoadSettings Strict = new JsonLoadSettings
        {
            DuplicatePropertyNameHandling = DuplicatePropertyNameHandling.Error,
        };

        /// <summary>What a game hands over as the text of JSON, read whole: the value, and nothing after it.</summary>
        public static JToken Value(string json, string what)
        {
            if (json == null)
            {
                throw new ArgumentNullException(what);
            }

            try
            {
                using var reader = new JsonTextReader(new StringReader(json))
                {
                    DateParseHandling = DateParseHandling.None,
                };
                var value = JToken.ReadFrom(reader, Strict);
                if (!reader.Read())
                {
                    return value;
                }
            }
            catch (JsonException error)
            {
                throw new ArgumentException("not JSON: " + error.Message, what);
            }
            throw new ArgumentException("not JSON: text after the value", what);
        }

        /// <summary><see cref="Value"/> that is an object: a control message, props, a filter.</summary>
        public static JObject Object(string json, string what) =>
            Value(json, what) as JObject ?? throw new ArgumentException("not a JSON object", what);

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
