using System;
using System.Collections.Generic;
using System.Text;
using System.Threading.Tasks;
using Aurix.Net.Managed;

namespace Aurix.Net
{
    public struct ChatMessage { public ushort From; public ushort? Channel; public string Text; }

    /// <summary>Chat on top of a relay room, no extra service: channels are interest groups (Join = subscribe),
    /// messages are reliable events to the group cached for late joiners (history the room still holds), whispers are
    /// events to one player. Stays inside the room's moderation, rate limits and GDPR tooling.</summary>
    public sealed class ChatClient
    {
        readonly ManagedClient _client; readonly byte _code; readonly ushort _groupBase; readonly bool _history;
        readonly HashSet<ushort> _joined = new HashSet<ushort>();
        public event Action<ChatMessage> OnMessage;

        /// <param name="code">Relay event code reserved for chat (default 240).</param>
        /// <param name="groupBase">Keeps chat groups away from gameplay interest groups.</param>
        public ChatClient(ManagedClient client, byte code = 240, ushort groupBase = 60000, bool history = true)
        {
            _client = client; _code = code; _groupBase = groupBase; _history = history;
            client.OnEvent += HandleEvent;
        }
        ushort Group(ushort channel) => (ushort)(_groupBase + channel);

        public Task JoinAsync(ushort channel) => _joined.Add(channel) ? _client.SetGroupsAsync(new[] { Group(channel) }, null) : Task.CompletedTask;
        public Task LeaveAsync(ushort channel) => _joined.Remove(channel) ? _client.SetGroupsAsync(null, new[] { Group(channel) }) : Task.CompletedTask;
        /// <summary>Send to a channel; history keeps it in the room cache for late joiners.</summary>
        public void Send(ushort channel, string text, bool? history = null) =>
            _client.SendEvent(_code, Encode(0, channel, text), Target.All, true, null, Group(channel), (history ?? _history) ? CacheOp.Add : CacheOp.None);
        /// <summary>Private message to one player (never cached).</summary>
        public void Whisper(ushort actor, string text) => _client.SendEvent(_code, Encode(1, 0, text), Target.Actors, true, new[] { actor });

        static byte[] Encode(byte kind, ushort channel, string text)
        {
            var t = Encoding.UTF8.GetBytes(text.Length > 2000 ? text.Substring(0, 2000) : text);
            var b = new byte[3 + t.Length]; b[0] = kind; b[1] = (byte)(channel >> 8); b[2] = (byte)channel; Buffer.BlockCopy(t, 0, b, 3, t.Length); return b;
        }
        void HandleEvent(EventFrame f)
        {
            if (f.Code != _code || f.Payload.Count < 3) return;
            var p = f.Payload; byte kind = p.Array[p.Offset]; ushort channel = (ushort)((p.Array[p.Offset + 1] << 8) | p.Array[p.Offset + 2]);
            var text = Encoding.UTF8.GetString(p.Array, p.Offset + 3, p.Count - 3);
            if (kind == 0 && !_joined.Contains(channel)) return;
            OnMessage?.Invoke(new ChatMessage { From = f.From, Channel = kind == 1 ? (ushort?)null : channel, Text = text });
        }
    }
}
