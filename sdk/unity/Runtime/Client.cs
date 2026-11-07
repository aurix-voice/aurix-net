// Aurix Net client for Unity/.NET. Poll() once per frame from the main thread.
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

namespace Aurix.Net
{
    public enum Channel : byte { Unreliable = 0, Reliable = 1, ReliableUnordered = 2 }
    public enum Target { All = 0, Others = 1, Master = 2, Actors = 3 }
    public enum RoomMode { Relay, Sync, Lockstep }

    public sealed class NetException : Exception { public NetException(string m) : base(m) { } }

    /// <summary>A relay event frame decoded from the data path.</summary>
    public struct EventFrame
    {
        public byte Code;
        public ushort From;
        public ushort? Group;
        public ArraySegment<byte> Payload;

        /// <summary>Wire: u8 code, u8 flags, u16 from (BE), [u16 group if flags&0x10], [u16 n + n*u16 if target==3], payload.</summary>
        public static bool TryDecode(byte[] buf, out EventFrame f)
        {
            f = default;
            if (buf == null || buf.Length < 4) return false;
            f.Code = buf[0];
            byte flags = buf[1];
            f.From = (ushort)((buf[2] << 8) | buf[3]);
            int off = 4;
            if ((flags & 0x10) != 0) { if (buf.Length < off + 2) return false; f.Group = (ushort)((buf[off] << 8) | buf[off + 1]); off += 2; }
            if ((flags & 0x03) == 3)
            {
                if (buf.Length < off + 2) return false;
                int n = (buf[off] << 8) | buf[off + 1];
                off += 2 + n * 2;
                if (buf.Length < off) return false;
            }
            f.Payload = new ArraySegment<byte>(buf, off, buf.Length - off);
            return true;
        }
    }

    public sealed class Client : IDisposable
    {
        IntPtr _h;
        public bool IsConnected => _h != IntPtr.Zero;
        public int Actor { get; private set; } = -1;
        public string JoinedJson { get; private set; }

        /// <summary>Server control message as JSON (joined, player_joined, player_left, desync, ...).</summary>
        public event Action<string> OnControl;
        /// <summary>Data-path message: channel, tick, raw bytes (relay event frame / sync delta / lockstep bundle).</summary>
        public event Action<Channel, ushort, byte[]> OnData;
        public event Action OnDataReady;
        public event Action<string> OnClosed;

        public static string Version => Native.Utf8(Native.aurix_net_version());

        /// <summary>Backend/tooling only; never ship an API key in a game build.</summary>
        public static string IssueGrant(string http, string apiKey, string playerId, string name)
        {
            var p = Native.aurix_net_issue_grant(http, apiKey, playerId, name);
            if (p == IntPtr.Zero) throw new NetException(Native.LastError());
            var s = Native.Utf8(p); Native.aurix_net_string_free(p); return s;
        }

        /// <summary>Opens the control channel and authenticates. Blocks briefly (network round trip).</summary>
        public static Client Connect(string http, string grant, bool tunnel = false)
        {
            var h = Native.aurix_net_connect(http, grant, tunnel ? 1 : 0);
            if (h == IntPtr.Zero) throw new NetException(Native.LastError());
            return new Client { _h = h };
        }

        public void SendControl(string json)
        {
            if (Native.aurix_net_send_control(_h, json) != 0) throw new NetException(Native.LastError());
        }

        /// <summary>Joins or creates a room and opens the data path. Returns the actor id.</summary>
        public int JoinOrCreate(string name, RoomMode mode = RoomMode.Relay, int maxPlayers = 8, string propsJson = null)
        {
            var sb = new StringBuilder("{\"t\":\"join_or_create\",\"name\":").Append(Json(name))
                .Append(",\"mode\":\"").Append(mode.ToString().ToLowerInvariant()).Append("\",\"max_players\":").Append(maxPlayers);
            if (!string.IsNullOrEmpty(propsJson)) sb.Append(",\"props\":").Append(propsJson);
            return Join(sb.Append('}').ToString());
        }

        public int Join(string name, string schemaHash = null)
        {
            var sb = new StringBuilder("{\"t\":\"join\",\"name\":").Append(Json(name));
            if (schemaHash != null) sb.Append(",\"schema_hash\":").Append(Json(schemaHash));
            return Join(sb.Append('}').ToString());
        }

        int Join(string json)
        {
            var a = Native.aurix_net_join(_h, json);
            if (a < 0) throw new NetException(Native.LastError());
            Actor = a;
            var p = Native.aurix_net_joined_json(_h);
            JoinedJson = Native.Utf8(p); Native.aurix_net_string_free(p);
            return a;
        }

        public void SendEvent(byte code, byte[] data, Target target = Target.Others, bool reliable = false, ushort[] actors = null)
        {
            var r = Native.aurix_net_send_event(_h, code, (int)target, actors, (UIntPtr)(actors?.Length ?? 0), data, (UIntPtr)(data?.Length ?? 0), reliable ? 1 : 0);
            if (r != 0) throw new NetException(Native.LastError());
        }

        public void SendData(Channel channel, byte[] data)
        {
            if (Native.aurix_net_send_data(_h, (byte)channel, data, (UIntPtr)(data?.Length ?? 0)) != 0) throw new NetException(Native.LastError());
        }

        /// <summary>Pushes queued unreliable messages onto the wire; call at the end of the frame.</summary>
        public void Flush() => Native.aurix_net_flush(_h);

        /// <summary>Dispatches pending events (up to maxEvents). Call every frame.</summary>
        public int Poll(int maxEvents = 256)
        {
            int n = 0;
            while (n < maxEvents && Native.aurix_net_poll(_h, out var ev) == 1)
            {
                n++;
                try
                {
                    switch (ev.kind)
                    {
                        case Native.EventControl: OnControl?.Invoke(Native.Utf8(ev.json)); break;
                        case Native.EventData:
                            var len = (int)ev.dataLen;
                            var buf = new byte[len];
                            if (len > 0) Marshal.Copy(ev.data, buf, 0, len);
                            OnData?.Invoke((Channel)ev.channel, ev.tick, buf);
                            break;
                        case Native.EventDataReady: OnDataReady?.Invoke(); break;
                        case Native.EventClosed: OnClosed?.Invoke(Native.Utf8(ev.json)); break;
                    }
                }
                finally { Native.aurix_net_event_free(ref ev); }
            }
            return n;
        }

        public void Leave() { if (_h != IntPtr.Zero) Native.aurix_net_leave(_h); }

        public void Dispose()
        {
            if (_h != IntPtr.Zero) { Native.aurix_net_destroy(_h); _h = IntPtr.Zero; }
        }

        static string Json(string s)
        {
            var sb = new StringBuilder("\"");
            foreach (var c in s)
            {
                if (c == '"' || c == '\\') sb.Append('\\').Append(c);
                else if (c < 0x20) sb.Append("\\u").Append(((int)c).ToString("x4"));
                else sb.Append(c);
            }
            return sb.Append('"').ToString();
        }
    }
}
