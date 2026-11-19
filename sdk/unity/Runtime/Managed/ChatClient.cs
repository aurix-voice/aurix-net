using System;
using System.Collections.Generic;
using System.Text;
using System.Threading.Tasks;
using Aurix.Net.Managed;

namespace Aurix.Net
{
    public struct ChatMessage
    {
        public ushort From;
        public ushort? Channel;
        public string Text;

        /// <summary>History the room kept for late joiners, not a message sent just now.</summary>
        public bool Cached;
    }

    /// <summary>Chat on top of a relay room, no extra service: channels are interest groups (Join = subscribe),
    /// messages are reliable events to the group cached for late joiners (history the room still holds), whispers are
    /// events to one player. Stays inside the room's moderation, rate limits and GDPR tooling.</summary>
    public sealed class ChatClient
    {
        readonly ManagedClient _client;
        readonly byte _code;
        readonly ushort _groupBase;
        readonly bool _history;
        readonly HashSet<ushort> _joined = new HashSet<ushort>();
        int _stay;
        public event Action<ChatMessage> OnMessage;

        /// <summary>The longest chat text, in Unicode characters (code points); a longer one is refused.</summary>
        public const int MaxText = 2000;

        /// <param name="code">Relay event code reserved for chat (default 240).</param>
        /// <param name="groupBase">Keeps chat groups away from gameplay interest groups.</param>
        public ChatClient(
            ManagedClient client,
            byte code = 240,
            ushort groupBase = 60000,
            bool history = true
        )
        {
            _client = client;
            _code = code;
            _groupBase = groupBase;
            _history = history;
            client.OnEvent += HandleEvent;
        }

        ushort Group(ushort channel)
        {
            int group = _groupBase + channel;
            if (group > ushort.MaxValue)
            {
                throw new ArgumentOutOfRangeException(
                    nameof(channel),
                    "chat channel " + channel + " is group " + group + ", not in 0..65535"
                );
            }

            return (ushort)group;
        }

        // channels are groups of a room: out of it the chat is in none
        HashSet<ushort> Channels()
        {
            if (_stay != _client.Stay)
            {
                _stay = _client.Stay;
                _joined.Clear();
            }

            return _joined;
        }

        public Task JoinAsync(ushort channel)
        {
            ushort group = Group(channel);
            if (!Channels().Add(channel))
            {
                return Task.CompletedTask;
            }

            foreach (var frame in _client.Cached)
            {
                var payload = frame.Payload;
                if (
                    frame.Code == _code
                    && payload.Count >= 3
                    && payload.Array[payload.Offset] == 0
                    && (
                        (payload.Array[payload.Offset + 1] << 8) | payload.Array[payload.Offset + 2]
                    ) == channel
                )
                {
                    HandleEvent(frame);
                }
            }
            return _client.SetGroupsAsync(new[] { group }, null);
        }

        public Task LeaveAsync(ushort channel) =>
            Channels().Remove(channel)
                ? _client.SetGroupsAsync(null, new[] { Group(channel) })
                : Task.CompletedTask;

        /// <summary>Send to a channel; history keeps it in the room cache for late joiners.</summary>
        public void Send(ushort channel, string text, bool? history = null) =>
            _client.SendEvent(
                _code,
                Encode(0, channel, text),
                Target.All,
                true,
                null,
                Group(channel),
                (history ?? _history) ? CacheOp.Add : CacheOp.None
            );

        /// <summary>Private message to one player (never cached).</summary>
        public void Whisper(ushort actor, string text) =>
            _client.SendEvent(_code, Encode(1, 0, text), Target.Actors, true, new[] { actor });

        static byte[] Encode(byte kind, ushort channel, string text)
        {
            int characters = 0;
            for (int i = 0; i < text.Length; i++, characters++)
            {
                if (
                    char.IsHighSurrogate(text[i])
                    && i + 1 < text.Length
                    && char.IsLowSurrogate(text[i + 1])
                )
                {
                    i++;
                }
            }

            if (characters > MaxText)
            {
                throw new ArgumentOutOfRangeException(
                    nameof(text),
                    "chat text of " + characters + " characters: at most " + MaxText
                );
            }

            var encoded = Encoding.UTF8.GetBytes(text);
            var bytes = new byte[3 + encoded.Length];
            bytes[0] = kind;
            bytes[1] = (byte)(channel >> 8);
            bytes[2] = (byte)channel;
            Buffer.BlockCopy(encoded, 0, bytes, 3, encoded.Length);
            return bytes;
        }

        void HandleEvent(EventFrame frame)
        {
            if (frame.Code != _code || frame.Payload.Count < 3)
            {
                return;
            }

            var payload = frame.Payload;
            byte kind = payload.Array[payload.Offset];
            ushort channel = (ushort)(
                (payload.Array[payload.Offset + 1] << 8) | payload.Array[payload.Offset + 2]
            );
            var text = Encoding.UTF8.GetString(
                payload.Array,
                payload.Offset + 3,
                payload.Count - 3
            );
            if (kind == 0 && !Channels().Contains(channel))
            {
                return;
            }

            OnMessage?.Invoke(
                new ChatMessage
                {
                    From = frame.From,
                    Channel = kind == 1 ? (ushort?)null : channel,
                    Text = text,
                    Cached = frame.Cached,
                }
            );
        }
    }
}
