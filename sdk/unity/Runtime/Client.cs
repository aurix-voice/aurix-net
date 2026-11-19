// Aurix Net client for Unity/.NET. Poll() once per frame from the main thread.
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

namespace Aurix.Net
{
    public enum Channel : byte
    {
        /// <summary>Unreliable; a message that arrives after a newer one is dropped.</summary>
        Unreliable = 0,
        Reliable = 1,
        ReliableUnordered = 2,

        /// <summary>Unreliable; every message that arrives is delivered, in the order of arrival.</summary>
        UnreliableUnsequenced = 4,
    }

    public enum Target
    {
        All = 0,
        Others = 1,
        Master = 2,
        Actors = 3,
    }

    public enum RoomMode
    {
        Relay,
        Sync,
        Lockstep,
    }

    public sealed class NetException : Exception
    {
        public NetException(string message)
            : base(message) { }
    }

    /// <summary>A relay event frame decoded from the data path.</summary>
    public struct EventFrame
    {
        public byte Code;
        public ushort From;
        public ushort? Group;
        public ArraySegment<byte> Payload;

        /// <summary>Set by the managed client: the frame arrived on a reliable channel.</summary>
        public bool Reliable;

        /// <summary>Set by the managed client: the room kept this event for late joiners and replayed it after the join.</summary>
        public bool Cached;

        /// <summary>Wire: u8 code, u8 flags, u16 from (BE), [u16 group if flags&0x10], [u16 n + n*u16 if target==3], payload.</summary>
        public static bool TryDecode(byte[] buf, out EventFrame frame)
        {
            frame = default;
            if (buf == null || buf.Length < 4)
            {
                return false;
            }

            frame.Code = buf[0];
            byte flags = buf[1];
            frame.From = (ushort)((buf[2] << 8) | buf[3]);
            int off = 4;
            if ((flags & 0x10) != 0)
            {
                if (buf.Length < off + 2)
                {
                    return false;
                }

                frame.Group = (ushort)((buf[off] << 8) | buf[off + 1]);
                off += 2;
            }
            if ((flags & 0x03) == 3)
            {
                if (buf.Length < off + 2)
                {
                    return false;
                }

                int count = (buf[off] << 8) | buf[off + 1];
                off += 2 + count * 2;
                if (buf.Length < off)
                {
                    return false;
                }
            }
            frame.Payload = new ArraySegment<byte>(buf, off, buf.Length - off);
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
            var pointer = Native.aurix_net_issue_grant(http, apiKey, playerId, name);
            if (pointer == IntPtr.Zero)
            {
                throw new NetException(Native.LastError());
            }

            var text = Native.Utf8(pointer);
            Native.aurix_net_string_free(pointer);
            return text;
        }

        /// <summary>Opens the control channel and authenticates. Blocks briefly (network round trip).</summary>
        public static Client Connect(string http, string grant, bool tunnel = false)
        {
            var handle = Native.aurix_net_connect(http, grant, tunnel ? 1 : 0);
            if (handle == IntPtr.Zero)
            {
                throw new NetException(Native.LastError());
            }

            return new Client { _h = handle };
        }

        public void SendControl(string json)
        {
            if (Native.aurix_net_send_control(_h, json) != 0)
            {
                throw new NetException(Native.LastError());
            }
        }

        /// <summary>Joins or creates a room and opens the data path. Returns the actor id.</summary>
        public int JoinOrCreate(
            string name,
            RoomMode mode = RoomMode.Relay,
            int maxPlayers = 8,
            string propsJson = null,
            string[] reserved = null
        )
        {
            var sb = new StringBuilder("{\"t\":\"join_or_create\",\"name\":")
                .Append(Json(name))
                .Append(",\"mode\":\"")
                .Append(mode.ToString().ToLowerInvariant())
                .Append("\",\"max_players\":")
                .Append(maxPlayers);
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

                    sb.Append(Json(reserved[i]));
                }
                sb.Append(']');
            }
            return Join(sb.Append('}').ToString());
        }

        /// <summary>Joins a random open room. <paramref name="filterJson"/> must equal room props exactly,
        /// <paramref name="expr"/> is a SQL-like condition on lobby props ("map = 'dust' AND skill <= 1500"),
        /// <paramref name="matchType"/> is "fill" (default), "serial" or "random".</summary>
        public int JoinRandom(string filterJson = null, string expr = null, string matchType = null)
        {
            var sb = new StringBuilder("{\"t\":\"join_random\"");
            if (!string.IsNullOrEmpty(filterJson))
            {
                sb.Append(",\"filter\":").Append(filterJson);
            }

            if (!string.IsNullOrEmpty(expr))
            {
                sb.Append(",\"expr\":").Append(Json(expr));
            }

            if (!string.IsNullOrEmpty(matchType))
            {
                sb.Append(",\"match_type\":").Append(Json(matchType));
            }

            return Join(sb.Append('}').ToString());
        }

        public int Join(string name, string schemaHash = null)
        {
            var sb = new StringBuilder("{\"t\":\"join\",\"name\":").Append(Json(name));
            if (schemaHash != null)
            {
                sb.Append(",\"schema_hash\":").Append(Json(schemaHash));
            }

            return Join(sb.Append('}').ToString());
        }

        int Join(string json)
        {
            var actor = Native.aurix_net_join(_h, json);
            if (actor < 0)
            {
                throw new NetException(Native.LastError());
            }

            Actor = actor;
            ReadRoom();
            return actor;
        }

        /// <summary>Takes from the core what it knows of the room; outside a room that is nothing.</summary>
        void ReadRoom()
        {
            var pointer = Native.aurix_net_joined_json(_h);
            JoinedJson = Native.Utf8(pointer);
            Native.aurix_net_string_free(pointer);
            if (JoinedJson == null)
            {
                Actor = -1;
            }
        }

        public void SendEvent(
            byte code,
            byte[] data,
            Target target = Target.Others,
            bool reliable = false,
            ushort[] actors = null
        )
        {
            var result = Native.aurix_net_send_event(
                _h,
                code,
                (int)target,
                actors,
                (UIntPtr)(actors?.Length ?? 0),
                data,
                (UIntPtr)(data?.Length ?? 0),
                reliable ? 1 : 0
            );
            if (result != 0)
            {
                throw new NetException(Native.LastError());
            }
        }

        /// <summary><see cref="SendEvent"/> on a channel of the caller's choice: <see cref="Channel.Unreliable"/>
        /// for state, where a newer event replaces an older one, <see cref="Channel.UnreliableUnsequenced"/>
        /// for events that are all wanted. A node that does not know the unsequenced channel gets them on
        /// the sequenced one.</summary>
        public void SendEventOn(
            Channel channel,
            byte code,
            byte[] data,
            Target target = Target.Others,
            ushort[] actors = null
        )
        {
            var result = Native.aurix_net_send_event_on(
                _h,
                (byte)channel,
                code,
                (int)target,
                actors,
                (UIntPtr)(actors?.Length ?? 0),
                data,
                (UIntPtr)(data?.Length ?? 0),
                -1,
                0
            );
            if (result != 0)
            {
                throw new NetException(Native.LastError());
            }
        }

        public void SendData(Channel channel, byte[] data)
        {
            if (
                Native.aurix_net_send_data(_h, (byte)channel, data, (UIntPtr)(data?.Length ?? 0))
                != 0
            )
            {
                throw new NetException(Native.LastError());
            }
        }

        /// <summary>Pushes queued unreliable messages onto the wire; call at the end of the frame.</summary>
        public void Flush() => Native.aurix_net_flush(_h);

        /// <summary>Dispatches pending events (up to maxEvents). Call every frame.</summary>
        public int Poll(int maxEvents = 256)
        {
            int count = 0;
            while (count < maxEvents && Native.aurix_net_poll(_h, out var ev) == 1)
            {
                count++;
                try
                {
                    switch (ev.kind)
                    {
                        case Native.EventControl:
                            var json = Native.Utf8(ev.json);
                            if (Managed.Json.Str(json, "t") == "left")
                            {
                                ReadRoom();
                            }

                            OnControl?.Invoke(json);
                            break;
                        case Native.EventData:
                            var len = (int)ev.dataLen;
                            var buf = new byte[len];
                            if (len > 0)
                            {
                                Marshal.Copy(ev.data, buf, 0, len);
                            }

                            OnData?.Invoke((Channel)ev.channel, ev.tick, buf);
                            break;
                        case Native.EventDataReady:
                            OnDataReady?.Invoke();
                            break;
                        case Native.EventClosed:
                            OnClosed?.Invoke(Native.Utf8(ev.json));
                            break;
                    }
                }
                finally
                {
                    Native.aurix_net_event_free(ref ev);
                }
            }
            return count;
        }

        public void Leave()
        {
            if (_h != IntPtr.Zero)
            {
                Native.aurix_net_leave(_h);
                ReadRoom();
            }
        }

        public void Dispose()
        {
            if (_h != IntPtr.Zero)
            {
                Native.aurix_net_destroy(_h);
                _h = IntPtr.Zero;
            }
        }

        static string Json(string text)
        {
            var sb = new StringBuilder("\"");
            foreach (var character in text)
            {
                if (character == '"' || character == '\\')
                {
                    sb.Append('\\').Append(character);
                }
                else if (character < 0x20)
                {
                    sb.Append("\\u").Append(((int)character).ToString("x4"));
                }
                else
                {
                    sb.Append(character);
                }
            }
            return sb.Append('"').ToString();
        }
    }
}
