// Pure C# Aurix Net client: no native library, works on every Unity platform including WebGL
// (via the WebSocket tunnel). Control channel = WebSocket JSON, data = WebSocket binary tunnel
// using the same packet format as QUIC. Call Poll() once per frame from the main thread.
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Linq;
using System.Net.WebSockets;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace Aurix.Net.Managed
{
    public enum CacheOp : byte { None = 0, Add = 1, Remove = 2 }

    public sealed class ManagedClient : IDisposable
    {
        ClientWebSocket _ws;
        readonly Link _link = new Link();
        readonly ConcurrentQueue<object> _inbox = new ConcurrentQueue<object>();
        readonly SemaphoreSlim _sendLock = new SemaphoreSlim(1, 1);
        CancellationTokenSource _cts;
        bool _dataReady;

        public int Actor { get; private set; } = -1;
        /// <summary>URL given to ConnectAsync, the grant and the last join message: what RejoinAsync needs.</summary>
        public string EntryUrl { get; private set; }
        public string Grant { get; private set; }
        public string LastJoinJson { get; private set; }
        /// <summary>The node shut down and handed the room to another node: call RejoinAsync (NetRunner does).</summary>
        public event Action OnRehost;
        /// <summary>System notice from the operator or game backend: (text, level = "info" | "warn").</summary>
        public event Action<string, string> OnNotice;
        public string JoinedJson { get; private set; }
        public string RoomMode { get; private set; }

        /// <summary>Raw control message (JSON text).</summary>
        public event Action<string> OnControl;
        /// <summary>Data-path message (channel, server tick, payload).</summary>
        public event Action<Channel, ushort, byte[]> OnData;
        /// <summary>Relay event decoded from the data path.</summary>
        public event Action<EventFrame> OnEvent;
        public event Action OnDataReady;
        public event Action<string> OnClosed;

        sealed class Closed { public string Reason; }
        sealed class Text { public string Json; }
        sealed class Bin { public byte[] Data; }

        /// <summary>Opens the control channel and authenticates with a grant.</summary>
        public static async Task<ManagedClient> ConnectAsync(string httpUrl, string grant, int timeoutMs = 10000)
        {
            var c = new ManagedClient { _ws = new ClientWebSocket(), _cts = new CancellationTokenSource(), EntryUrl = httpUrl, Grant = grant };
            var url = (httpUrl.StartsWith("https") ? "wss" : "ws") + httpUrl.Substring(httpUrl.IndexOf(':')) + "/ws";
            using (var t = new CancellationTokenSource(timeoutMs)) await c._ws.ConnectAsync(new Uri(url), t.Token);
            _ = c.ReadLoop();
            await c.SendControlAsync("{\"t\":\"auth\",\"grant\":" + Json.Str(grant) + "}");
            await c.WaitForAsync("authed", timeoutMs);
            return c;
        }

        /// <summary>connect + join, following one `room_elsewhere:&lt;url&gt;` redirect.</summary>
        public static async Task<ManagedClient> ConnectAndJoinAsync(string httpUrl, string grant, string joinJson, int timeoutMs = 10000)
        {
            var c = await ConnectAsync(httpUrl, grant, timeoutMs);
            try { await c.JoinRawAsync(joinJson, timeoutMs); return c; }
            catch (NetException e) when (e.Message.Contains("room_elsewhere:"))
            {
                c.Dispose();
                var url = e.Message.Substring(e.Message.IndexOf("room_elsewhere:") + 15).Trim();
                var c2 = await ConnectAsync(url, grant, timeoutMs);
                await c2.JoinRawAsync(joinJson, timeoutMs);
                return c2;
            }
        }

        public Task<int> JoinOrCreateAsync(string name, string mode = "relay", int maxPlayers = 8, string propsJson = null, string[] reserved = null)
        {
            var sb = new StringBuilder("{\"t\":\"join_or_create\",\"name\":").Append(Json.Str(name))
                .Append(",\"mode\":\"").Append(mode).Append("\",\"max_players\":").Append(maxPlayers);
            if (!string.IsNullOrEmpty(propsJson)) sb.Append(",\"props\":").Append(propsJson);
            if (reserved != null && reserved.Length > 0)
            {
                sb.Append(",\"reserved\":[");
                for (int i = 0; i < reserved.Length; i++) { if (i > 0) sb.Append(','); sb.Append(Json.Str(reserved[i])); }
                sb.Append(']');
            }
            return JoinRawAsync(sb.Append('}').ToString());
        }

        /// <summary>Joins a random open room. <paramref name="filterJson"/> must equal room props exactly,
        /// <paramref name="expr"/> is a SQL-like condition on lobby props ("map = 'dust' AND skill <= 1500"),
        /// <paramref name="matchType"/> is "fill" (default), "serial" or "random".</summary>
        public Task<int> JoinRandomAsync(string filterJson = null, string expr = null, string matchType = null)
        {
            var sb = new StringBuilder("{\"t\":\"join_random\"");
            if (!string.IsNullOrEmpty(filterJson)) sb.Append(",\"filter\":").Append(filterJson);
            if (!string.IsNullOrEmpty(expr)) sb.Append(",\"expr\":").Append(Json.Str(expr));
            if (!string.IsNullOrEmpty(matchType)) sb.Append(",\"match_type\":").Append(Json.Str(matchType));
            return JoinRawAsync(sb.Append('}').ToString());
        }

        public Task<int> JoinAsync(string name, string schemaHash = null, bool spectator = false)
        {
            var sb = new StringBuilder("{\"t\":\"join\",\"name\":").Append(Json.Str(name));
            if (schemaHash != null) sb.Append(",\"schema_hash\":").Append(Json.Str(schemaHash));
            if (spectator) sb.Append(",\"spectator\":true");
            return JoinRawAsync(sb.Append('}').ToString());
        }

        /// <summary>Any join-like control message; opens the data path (ticket first) and waits for the server's ack.</summary>
        /// <summary>After a rehost: a fresh client in the same room with the same grant (entry URL, then fallbacks).</summary>
        public async Task<ManagedClient> RejoinAsync(params string[] fallbackUrls)
        {
            if (LastJoinJson == null) throw new NetException("rejoin: never joined");
            var msg = LastJoinJson.Replace("\"t\":\"join_or_create\"", "\"t\":\"join\"");
            Dispose();
            Exception last = null;
            foreach (var url in new[] { EntryUrl }.Concat(fallbackUrls))
            {
                try { return await ConnectAndJoinAsync(url, Grant, msg); } catch (Exception e) { last = e; }
            }
            throw last;
        }

        public async Task<int> JoinRawAsync(string json, int timeoutMs = 10000)
        {
            LastJoinJson = json;
            await SendControlAsync(json);
            var joined = await WaitForAsync("joined", timeoutMs);
            JoinedJson = joined;
            Actor = (int)Json.Num(joined, "actor");
            RoomMode = Json.Str(joined, "mode");
            var data = Json.Obj(joined, "data");
            if (Json.Bool(data, "ws_tunnel") == false) throw new NetException("node has the WebSocket tunnel disabled");
            var ticket = Convert.FromBase64String(Json.Str(data, "ticket"));
            await SendBinaryAsync(ticket);
            var deadline = DateTime.UtcNow.AddMilliseconds(timeoutMs);
            while (!_dataReady) { if (DateTime.UtcNow > deadline) throw new NetException("data path timeout"); await Task.Delay(5); Poll(); }
            return Actor;
        }

        public Task SendControlAsync(string json) => SendAsync(Encoding.UTF8.GetBytes(json), WebSocketMessageType.Text);
        Task SendBinaryAsync(byte[] b) => SendAsync(b, WebSocketMessageType.Binary);
        async Task SendAsync(byte[] b, WebSocketMessageType kind)
        {
            await _sendLock.WaitAsync();
            try { if (_ws.State == WebSocketState.Open) await _ws.SendAsync(new ArraySegment<byte>(b), kind, true, _cts.Token); }
            finally { _sendLock.Release(); }
        }

        public void SendData(Channel channel, byte[] payload) { _link.Send(channel, payload); if (channel != Channel.Unreliable) FlushLink(); }
        /// <param name="cache">Add keeps the event for late joiners (current cache slice, see CacheSliceAsync); Remove drops your earlier cached events with the same code.</param>
        public void SendEvent(byte code, byte[] payload, Target target = Target.Others, bool reliable = false, ushort[] actors = null, ushort? group = null, CacheOp cache = CacheOp.None)
            => SendData(reliable || cache != CacheOp.None ? Channel.Reliable : Channel.Unreliable, Event.Encode(code, payload, target, actors, group, cache));
        /// <summary>Relay event-cache slices: "increase" opens the next slice, "set" picks one, "purge" drops one slice, "purge_up_to" every slice up to index.</summary>
        public Task CacheSliceAsync(string op, ushort? index = null) =>
            SendControlAsync("{\"t\":\"cache_slice\",\"op\":\"" + op + "\"" + (index.HasValue ? ",\"index\":" + index.Value : "") + "}");

        /// <summary>Pushes queued unreliable messages; call at the end of the frame.</summary>
        public void Flush() { _link.FlushBatch(); FlushLink(); }

        void FlushLink() { while (_link.TryDequeueOutgoing(out var d)) _ = SendBinaryAsync(d); }

        /// <summary>Dispatches pending messages on the calling thread. Call every frame.</summary>
        public int Poll(int max = 256)
        {
            int n = 0;
            if (!_link.Poll()) { _inbox.Enqueue(new Closed { Reason = "link dead" }); }
            FlushLink();
            while (n < max && _inbox.TryDequeue(out var m))
            {
                n++;
                switch (m)
                {
                    case Text t:
                        OnControl?.Invoke(t.Json);
                        if (Json.Str(t.Json, "t") == "left" && Json.Str(t.Json, "reason") == "rehost") OnRehost?.Invoke();
                        if (Json.Str(t.Json, "t") == "notice") OnNotice?.Invoke(Json.Str(t.Json, "text"), Json.Str(t.Json, "level"));
                        break;
                    case Bin b:
                        foreach (var r in _link.Receive(b.Data))
                        {
                            if (r.Payload.Length == 0) { if (!_dataReady) { _dataReady = true; OnDataReady?.Invoke(); } continue; }
                            OnData?.Invoke(r.Channel, r.Tick, r.Payload);
                            byte k = (byte)(r.Payload[0] & 0xf0);
                            if (RoomMode == "relay" || (k != 0x10 && k != 0x20))
                                if (Event.TryDecode(r.Payload, out var f)) OnEvent?.Invoke(f);
                        }
                        FlushLink();
                        break;
                    case Closed c: OnClosed?.Invoke(c.Reason); break;
                }
            }
            return n;
        }

        public Task LeaveAsync() => SendControlAsync("{\"t\":\"leave\"}");
        /// <summary>Sync rooms: steer area-of-interest — extra viewpoints (camera, scope target) and entities forced into view (team mates, objectives). Entity ids as in deltas.</summary>
        /// <summary>Project-wide live rooms/players grouped by the room prop "lobby" (JSON: {rooms, players, lobbies:[{lobby, rooms, players}]}); one per second.</summary>
        public async Task<string> StatsAsync(int timeoutMs = 10000)
        {
            await SendControlAsync("{\"t\":\"stats\"}");
            return await WaitForAsync("stats", timeoutMs);
        }

        /// <summary>Presence of up to 64 players across the fleet (JSON: {friends:[{player, online, room?:{id,name,mode,node}}]}); one per second.</summary>
        public async Task<string> FindFriendsAsync(string[] players, int timeoutMs = 10000)
        {
            var ids = new System.Text.StringBuilder();
            foreach (var p in players ?? Array.Empty<string>()) { if (ids.Length > 0) ids.Append(','); ids.Append(Json.Str(p)); }
            await SendControlAsync("{\"t\":\"find_friends\",\"players\":[" + ids + "]}");
            return await WaitForAsync("friends", timeoutMs);
        }

        /// <summary>One-shot room list (JSON {rooms:[{id,name,mode,players,max_players,open,props}]}), optionally filtered by a SQL-like expr on lobby props; one per second.</summary>
        public async Task<string> RoomsAsync(string expr = null, int limit = 100, int timeoutMs = 10000)
        {
            await SendControlAsync("{\"t\":\"get_rooms\",\"expr\":" + (expr == null ? "null" : Json.Str(expr)) + ",\"limit\":" + limit + "}");
            return await WaitForAsync("rooms", timeoutMs);
        }

        int _rpcSeq;
        /// <summary>Call your backend through the node (WebRPC): the project's webhook subscribed to "rpc" is POSTed {name, player_id, actor?, room?, body}; returns the raw rpc_result JSON {id, ok, status?, body, error?}. bodyJson is a JSON literal (≤ 8 KiB); 5 calls/s.</summary>
        public async Task<string> RpcAsync(string name, string bodyJson = "null", int timeoutMs = 10000)
        {
            var id = (uint)System.Threading.Interlocked.Increment(ref _rpcSeq);
            await SendControlAsync("{\"t\":\"rpc\",\"id\":" + id + ",\"name\":" + Json.Str(name) + ",\"body\":" + (string.IsNullOrEmpty(bodyJson) ? "null" : bodyJson) + "}");
            return await WaitForAsync("rpc_result", timeoutMs);
        }

        public Task SetViewAsync(ulong[] viewpoints = null, ulong[] interest = null) =>
            SendControlAsync("{\"t\":\"set_view\",\"viewpoints\":[" + string.Join(",", viewpoints ?? Array.Empty<ulong>()) + "],\"interest\":[" + string.Join(",", interest ?? Array.Empty<ulong>()) + "]}");

        async Task ReadLoop()
        {
            var buf = new byte[64 * 1024]; var acc = new List<byte>();
            try
            {
                while (_ws.State == WebSocketState.Open)
                {
                    var r = await _ws.ReceiveAsync(new ArraySegment<byte>(buf), _cts.Token);
                    if (r.MessageType == WebSocketMessageType.Close) break;
                    for (int i = 0; i < r.Count; i++) acc.Add(buf[i]);
                    if (!r.EndOfMessage) continue;
                    var msg = acc.ToArray(); acc.Clear();
                    if (r.MessageType == WebSocketMessageType.Text) _inbox.Enqueue(new Text { Json = Encoding.UTF8.GetString(msg) });
                    else _inbox.Enqueue(new Bin { Data = msg });
                }
                _inbox.Enqueue(new Closed { Reason = "control channel closed" });
            }
            catch (Exception e) { if (!_cts.IsCancellationRequested) _inbox.Enqueue(new Closed { Reason = e.Message }); }
        }

        /// <summary>Waits for a control message of type `t`, polling the inbox (other messages are dispatched normally).</summary>
        async Task<string> WaitForAsync(string t, int timeoutMs)
        {
            var deadline = DateTime.UtcNow.AddMilliseconds(timeoutMs);
            string got = null, err = null;
            Action<string> h = j => { var ty = Json.Str(j, "t"); if (ty == t) got = j; else if (ty == "error") err = Json.Str(j, "code") + ": " + Json.Str(j, "message"); };
            OnControl += h;
            try
            {
                while (got == null && err == null) { if (DateTime.UtcNow > deadline) throw new NetException("timeout waiting for " + t); Poll(); await Task.Delay(2); }
            }
            finally { OnControl -= h; }
            if (err != null) throw new NetException(err);
            return got;
        }

        public void Dispose()
        {
            try { _cts?.Cancel(); _ws?.Dispose(); } catch { }
        }
    }

    /// <summary>Relay event frame codec (u8 code, u8 flags, u16 from, [u16 group], [u16 n, n*u16], payload).</summary>
    public static class Event
    {
        public static byte[] Encode(byte code, byte[] payload, Target target, ushort[] actors = null, ushort? group = null, CacheOp cache = CacheOp.None)
        {
            int n = target == Target.Actors ? (actors?.Length ?? 0) : 0;
            var b = new byte[4 + (group.HasValue ? 2 : 0) + (target == Target.Actors ? 2 + n * 2 : 0) + (payload?.Length ?? 0)];
            int o = 0; b[o++] = code; b[o++] = (byte)((int)target | (group.HasValue ? 0x10 : 0) | (cache == CacheOp.Add ? 0x04 : cache == CacheOp.Remove ? 0x08 : 0)); b[o++] = 0; b[o++] = 0;
            if (group.HasValue) { b[o++] = (byte)(group.Value >> 8); b[o++] = (byte)group.Value; }
            if (target == Target.Actors) { b[o++] = (byte)(n >> 8); b[o++] = (byte)n; for (int i = 0; i < n; i++) { b[o++] = (byte)(actors[i] >> 8); b[o++] = (byte)actors[i]; } }
            if (payload != null) Array.Copy(payload, 0, b, o, payload.Length);
            return b;
        }
        public static bool TryDecode(byte[] buf, out EventFrame f) => EventFrame.TryDecode(buf, out f);
    }

    /// <summary>Minimal JSON field access for the handful of control fields the client needs.</summary>
    internal static class Json
    {
        public static string Str(string s)
        {
            var sb = new StringBuilder("\"");
            foreach (var c in s) { if (c == '"' || c == '\\') sb.Append('\\').Append(c); else if (c < 0x20) sb.Append("\\u").Append(((int)c).ToString("x4")); else sb.Append(c); }
            return sb.Append('"').ToString();
        }
        static int Find(string json, string key)
        {
            if (json == null) return -1;
            int i = json.IndexOf("\"" + key + "\"", StringComparison.Ordinal);
            if (i < 0) return -1;
            i = json.IndexOf(':', i + key.Length + 2); return i < 0 ? -1 : i + 1;
        }
        public static string Str(string json, string key)
        {
            int i = Find(json, key); if (i < 0) return null;
            while (i < json.Length && json[i] == ' ') i++;
            if (i >= json.Length || json[i] != '"') return null;
            var sb = new StringBuilder();
            for (i++; i < json.Length; i++)
            {
                char c = json[i];
                if (c == '"') break;
                if (c == '\\' && i + 1 < json.Length) { c = json[++i]; if (c == 'u' && i + 4 < json.Length) { sb.Append((char)Convert.ToInt32(json.Substring(i + 1, 4), 16)); i += 4; continue; } if (c == 'n') c = '\n'; }
                sb.Append(c);
            }
            return sb.ToString();
        }
        public static double Num(string json, string key)
        {
            int i = Find(json, key); if (i < 0) return double.NaN;
            int j = i; while (j < json.Length && (char.IsDigit(json[j]) || json[j] == '-' || json[j] == '.' || json[j] == ' ')) j++;
            return double.TryParse(json.Substring(i, j - i).Trim(), System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var v) ? v : double.NaN;
        }
        public static bool? Bool(string json, string key)
        {
            int i = Find(json, key); if (i < 0) return null;
            var rest = json.Substring(i).TrimStart();
            return rest.StartsWith("true") ? true : rest.StartsWith("false") ? false : (bool?)null;
        }
        /// <summary>Substring of the object value for `key` (brace matched).</summary>
        public static string Obj(string json, string key)
        {
            int i = Find(json, key); if (i < 0) return null;
            while (i < json.Length && json[i] != '{') i++;
            int depth = 0; bool inStr = false;
            for (int j = i; j < json.Length; j++)
            {
                char c = json[j];
                if (inStr) { if (c == '\\') j++; else if (c == '"') inStr = false; continue; }
                if (c == '"') inStr = true; else if (c == '{') depth++; else if (c == '}' && --depth == 0) return json.Substring(i, j - i + 1);
            }
            return null;
        }
    }
}

namespace Aurix.Net.Managed
{
    /// <summary>Small JSON helpers exposed for samples/tests (field access on control messages).</summary>
    public static class ManagedClientTestHooks
    {
        public static string Str(string json, string key) => Json.Str(json, key);
        public static double Num(string json, string key) => Json.Num(json, key);
    }
}
