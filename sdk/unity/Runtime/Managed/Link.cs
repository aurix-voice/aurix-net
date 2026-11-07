// Pure C# port of net-transport::Link (and sdk/web link.ts): unreliable-sequenced batches,
// reliable-ordered, reliable-unordered, fragmentation up to 64 KiB. Not thread-safe: drive it
// from one thread (the Unity main thread) and feed datagrams through Receive().
using System;
using System.Collections.Generic;

namespace Aurix.Net.Managed
{
    public struct Received { public Channel Channel; public ushort Tick; public byte[] Payload; }

    public sealed class Link
    {
        const int Window = 32, MaxRetries = 10; const double MinRto = 50, MaxRto = 2000;
        const int FragHdr = 5, FragPayload = Packet.MaxUnfragmented - FragHdr, MaxMessage = 64 * 1024;

        sealed class InFlight { public ushort Seq; public byte[] Payload; public double SentAt; public int Retries; }
        sealed class Stream
        {
            public byte Kind; public bool Ordered;
            public ushort TxSeq; public List<InFlight> InFlight = new List<InFlight>(); public Queue<byte[]> Pending = new Queue<byte[]>();
            public double Rtt = 100, RttVar = 50;
            public bool HasAck; public ushort RxAck; public uint RxAckBits; public ushort Next; public Dictionary<ushort, byte[]> Buffer = new Dictionary<ushort, byte[]>();
            public double Rto() => Math.Min(MaxRto, Math.Max(MinRto, Rtt + 4 * RttVar));
            public void OnAck(ushort ack, uint bits, double now)
            {
                InFlight.RemoveAll(p =>
                {
                    bool acked = p.Seq == ack;
                    if (!acked && Packet.SeqNewer(ack, p.Seq)) { int d = (ushort)(ack - p.Seq); acked = d <= 32 && ((bits >> (d - 1)) & 1) == 1; }
                    if (acked && p.Retries == 0) { double s = now - p.SentAt; RttVar = 0.75 * RttVar + 0.25 * Math.Abs(s - Rtt); Rtt = 0.875 * Rtt + 0.125 * s; }
                    return acked;
                });
            }
            /// <summary>Records a received seq for acks; false when already seen or too old.</summary>
            public bool Mark(ushort seq)
            {
                if (!HasAck) { HasAck = true; RxAck = seq; RxAckBits = 0; return true; }
                if (seq == RxAck) return false;
                if (Packet.SeqNewer(seq, RxAck)) { int sh = (ushort)(seq - RxAck); RxAckBits = sh >= 32 ? 0 : (RxAckBits << sh) | (1u << (sh - 1)); RxAck = seq; return true; }
                int d = (ushort)(RxAck - seq); if (d > 32) return false;
                uint bit = 1u << (d - 1); if ((RxAckBits & bit) != 0) return false; RxAckBits |= bit; return true;
            }
        }
        sealed class Partial { public Channel Channel; public byte[][] Parts; public int Got; public double Started; }

        ushort _seqTx; bool _hasSeqRx; ushort _seqRx;
        readonly List<byte[]> _batch = new List<byte[]>(); int _batchLen;
        readonly Dictionary<byte, Stream> _streams = new Dictionary<byte, Stream>();
        ushort _fragId; readonly Dictionary<ushort, Partial> _partial = new Dictionary<ushort, Partial>();
        readonly Queue<byte[]> _out = new Queue<byte[]>();
        public ushort Tick;
        public long Sent, Recv, Retransmits, Dropped;

        public Link()
        {
            _streams[1] = new Stream { Kind = 1, Ordered = true };
            _streams[2] = new Stream { Kind = 2, Ordered = false };
            _streams[Packet.KindFragment] = new Stream { Kind = Packet.KindFragment, Ordered = false };
        }

        static double Now() => System.Diagnostics.Stopwatch.GetTimestamp() * 1000.0 / System.Diagnostics.Stopwatch.Frequency;

        Packet.Header Header(byte kind, ushort seq, bool ackOnly = false, bool batch = false)
        {
            _streams.TryGetValue(kind, out var s); bool hasAck = s != null && s.HasAck;
            return new Packet.Header { Kind = kind, AckOnly = ackOnly, HasAck = hasAck, Batch = batch, Seq = seq, Ack = hasAck ? s.RxAck : (ushort)0, AckBits = hasAck ? s.RxAckBits : 0, Tick = Tick };
        }
        void Push(in Packet.Header h, ReadOnlySpan<byte> payload) { _out.Enqueue(Packet.Encode(h, payload)); Sent++; }

        public void Send(Channel channel, byte[] msg)
        {
            if (msg.Length > MaxMessage) throw new ArgumentException("message too large (max 64 KiB)");
            if (msg.Length > Packet.MaxUnfragmented)
            {
                int count = (msg.Length + FragPayload - 1) / FragPayload; if (count > 255) throw new ArgumentException("message too large");
                ushort id = _fragId++;
                for (int i = 0; i < count; i++)
                {
                    int off = i * FragPayload, len = Math.Min(msg.Length - off, FragPayload);
                    var p = new byte[FragHdr + len]; p[0] = (byte)(id >> 8); p[1] = (byte)id; p[2] = (byte)channel; p[3] = (byte)i; p[4] = (byte)count;
                    Array.Copy(msg, off, p, FragHdr, len);
                    _streams[Packet.KindFragment].Pending.Enqueue(p);
                }
                Pump(Now()); return;
            }
            if (channel == Channel.Unreliable)
            {
                if (_batchLen + 2 + msg.Length > Packet.MaxUnfragmented) FlushBatch();
                _batch.Add(msg); _batchLen += 2 + msg.Length; return;
            }
            _streams[(byte)channel].Pending.Enqueue(msg); Pump(Now());
        }

        public void FlushBatch()
        {
            if (_batch.Count == 0) return;
            var p = new byte[_batchLen]; int o = 0;
            foreach (var m in _batch) { p[o++] = (byte)(m.Length >> 8); p[o++] = (byte)m.Length; Array.Copy(m, 0, p, o, m.Length); o += m.Length; }
            _batch.Clear(); _batchLen = 0;
            Push(Header((byte)Channel.Unreliable, _seqTx++, false, true), p);
        }

        void Pump(double now)
        {
            foreach (var s in _streams.Values)
                while (s.Pending.Count > 0 && s.InFlight.Count < Window)
                {
                    var payload = s.Pending.Dequeue(); ushort seq = s.TxSeq++;
                    s.InFlight.Add(new InFlight { Seq = seq, Payload = payload, SentAt = now, Retries = 0 });
                    Push(Header(s.Kind, seq), payload);
                }
        }

        /// <summary>Timer (call every few ms): retransmits and flushes the batch. False when the link is dead.</summary>
        public bool Poll()
        {
            double now = Now();
            FlushBatch();
            foreach (var s in _streams.Values)
            {
                double rto = s.Rto();
                foreach (var p in s.InFlight)
                    if (now - p.SentAt >= rto)
                    {
                        if (p.Retries >= MaxRetries) return false;
                        p.Retries++; p.SentAt = now; Retransmits++;
                        Push(Header(s.Kind, p.Seq), p.Payload);
                    }
            }
            if (_partial.Count > 0)
            {
                var stale = new List<ushort>();
                foreach (var kv in _partial) if (now - kv.Value.Started > 10000) stale.Add(kv.Key);
                foreach (var k in stale) _partial.Remove(k);
            }
            Pump(now);
            return true;
        }

        public bool TryDequeueOutgoing(out byte[] d) { if (_out.Count > 0) { d = _out.Dequeue(); return true; } d = null; return false; }

        bool Reassemble(ArraySegment<byte> frag, double now, ushort tick, out Received r)
        {
            r = default;
            if (frag.Count < FragHdr) return false;
            var b = frag.Array; int o = frag.Offset;
            ushort id = (ushort)((b[o] << 8) | b[o + 1]); var channel = (Channel)b[o + 2]; int index = b[o + 3], count = b[o + 4];
            if (count == 0 || index >= count) return false;
            if (!_partial.TryGetValue(id, out var p)) { if (_partial.Count >= 64) return false; p = new Partial { Channel = channel, Parts = new byte[count][], Started = now }; _partial[id] = p; }
            if (p.Parts.Length != count || p.Channel != channel) { _partial.Remove(id); return false; }
            if (p.Parts[index] == null) { var chunk = new byte[frag.Count - FragHdr]; Array.Copy(b, o + FragHdr, chunk, 0, chunk.Length); p.Parts[index] = chunk; p.Got++; }
            if (p.Got < count) return false;
            _partial.Remove(id);
            int total = 0; foreach (var x in p.Parts) total += x.Length;
            var msg = new byte[total]; int w = 0; foreach (var x in p.Parts) { Array.Copy(x, 0, msg, w, x.Length); w += x.Length; }
            r = new Received { Channel = channel, Tick = tick, Payload = msg }; return true;
        }

        public List<Received> Receive(byte[] d)
        {
            var outp = new List<Received>();
            if (!Packet.Decode(d, out var h, out var payload)) return outp;
            double now = Now(); Recv++;
            if (h.Kind == (byte)Channel.Unreliable)
            {
                if (!_hasSeqRx || Packet.SeqNewer(h.Seq, _seqRx))
                {
                    _hasSeqRx = true; _seqRx = h.Seq;
                    if (h.Batch)
                    {
                        int o = payload.Offset, end = payload.Offset + payload.Count; var b = payload.Array;
                        while (o + 2 <= end) { int n = (b[o] << 8) | b[o + 1]; o += 2; if (o + n > end) break; var m = new byte[n]; Array.Copy(b, o, m, 0, n); o += n; outp.Add(new Received { Channel = Channel.Unreliable, Tick = h.Tick, Payload = m }); }
                    }
                    else { var m = new byte[payload.Count]; Array.Copy(payload.Array, payload.Offset, m, 0, m.Length); outp.Add(new Received { Channel = Channel.Unreliable, Tick = h.Tick, Payload = m }); }
                }
                else Dropped++;
                return outp;
            }
            if (!_streams.TryGetValue(h.Kind, out var s)) return outp;
            if (h.HasAck) { s.OnAck(h.Ack, h.AckBits, now); Pump(now); }
            if (!h.AckOnly)
            {
                if (s.Mark(h.Seq))
                {
                    var copy = new byte[payload.Count]; Array.Copy(payload.Array, payload.Offset, copy, 0, copy.Length);
                    if (s.Ordered)
                    {
                        s.Buffer[h.Seq] = copy;
                        while (s.Buffer.TryGetValue(s.Next, out var m)) { Deliver(s, m, h.Tick, now, outp); s.Buffer.Remove(s.Next); s.Next++; }
                    }
                    else Deliver(s, copy, h.Tick, now, outp);
                }
                Push(Header(s.Kind, s.TxSeq, true), ReadOnlySpan<byte>.Empty);
            }
            return outp;
        }

        void Deliver(Stream s, byte[] p, ushort tick, double now, List<Received> outp)
        {
            if (s.Kind == Packet.KindFragment) { if (Reassemble(new ArraySegment<byte>(p), now, tick, out var r)) outp.Add(r); }
            else outp.Add(new Received { Channel = (Channel)s.Kind, Tick = tick, Payload = p });
        }
    }
}
