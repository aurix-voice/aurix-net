// Pure C# port of net-transport::Link (and sdk/web link.ts): unreliable-sequenced batches,
// reliable-ordered, reliable-unordered, fragmentation up to 64 KiB. Not thread-safe: drive it
// from one thread (the Unity main thread) and feed datagrams through Receive().
using System;
using System.Collections.Generic;

namespace Aurix.Net.Managed
{
    public struct Received
    {
        public Channel Channel;
        public ushort Tick;
        public byte[] Payload;
    }

    public sealed class Link
    {
        /// <summary>Idle timeout of the data path, in milliseconds, and of the link on it.</summary>
        public const double IdleTimeout = 15000;

        /// <summary>Bytes of reliable messages a link holds while they wait for the window. A sender
        /// that outruns its peer by more has lost it: the link ends instead of queueing without bound.</summary>
        public const int ReliableBacklogBytes = 1024 * 1024;

        const int Window = 32;
        const double MinRto = 60,
            MaxRto = 1500;
        const int FragHdr = 5,
            FragPayload = Packet.MaxUnfragmented - FragHdr,
            MaxMessage = 64 * 1024;

        sealed class InFlight
        {
            public ushort Seq;
            public byte[] Payload;
            public double SentAt;
            public double FirstSent;
            public bool Resent;
        }

        sealed class Stream
        {
            public byte Kind;
            public bool Ordered;
            public ushort TxSeq;
            public List<InFlight> InFlight = new List<InFlight>();
            public Queue<byte[]> Pending = new Queue<byte[]>();
            public int PendingBytes;
            public bool HasAck;
            public ushort RxAck;
            public uint RxAckBits;
            public ushort Next;
            public Dictionary<ushort, byte[]> Buffer = new Dictionary<ushort, byte[]>();

            /// <summary>Drops what <paramref name="ack"/> and <paramref name="bits"/> acknowledge;
            /// adds the round trips of the packets sent once to <paramref name="samples"/>.</summary>
            public void OnAck(ushort ack, uint bits, double now, List<double> samples)
            {
                InFlight.RemoveAll(packet =>
                {
                    bool acked = packet.Seq == ack;
                    if (!acked && Packet.SeqNewer(ack, packet.Seq))
                    {
                        int distance = (ushort)(ack - packet.Seq);
                        acked = distance <= 32 && ((bits >> (distance - 1)) & 1) == 1;
                    }
                    if (acked && !packet.Resent)
                    {
                        samples.Add(now - packet.SentAt);
                    }
                    return acked;
                });
            }

            /// <summary>Records a received seq for acks; false when already seen or too old.</summary>
            public bool Mark(ushort seq)
            {
                if (!HasAck)
                {
                    HasAck = true;
                    RxAck = seq;
                    RxAckBits = 0;
                    return true;
                }
                if (seq == RxAck)
                {
                    return false;
                }

                if (Packet.SeqNewer(seq, RxAck))
                {
                    int sh = (ushort)(seq - RxAck);
                    RxAckBits = sh >= 32 ? 0 : (RxAckBits << sh) | (1u << (sh - 1));
                    RxAck = seq;
                    return true;
                }
                int distance = (ushort)(RxAck - seq);
                if (distance > 32)
                {
                    return false;
                }

                uint bit = 1u << (distance - 1);
                if ((RxAckBits & bit) != 0)
                {
                    return false;
                }

                RxAckBits |= bit;
                return true;
            }
        }

        sealed class Partial
        {
            public Channel Channel;
            public byte[][] Parts;
            public int Got;
            public double Started;
        }

        /// <summary>The messages of one unreliable channel that leave in the next datagram.</summary>
        sealed class Batch
        {
            public ushort Seq;
            readonly List<byte[]> _messages = new List<byte[]>();
            int _length;

            public bool Fits(byte[] message) =>
                _length + 2 + message.Length <= Packet.MaxUnfragmented;

            public void Add(byte[] message)
            {
                _messages.Add(message);
                _length += 2 + message.Length;
            }

            public byte[] Take()
            {
                if (_messages.Count == 0)
                {
                    return null;
                }

                var payload = new byte[_length];
                int offset = 0;
                foreach (var message in _messages)
                {
                    payload[offset++] = (byte)(message.Length >> 8);
                    payload[offset++] = (byte)message.Length;
                    Array.Copy(message, 0, payload, offset, message.Length);
                    offset += message.Length;
                }
                _messages.Clear();
                _length = 0;
                return payload;
            }
        }

        bool _hasSeqRx;
        ushort _seqRx;
        readonly SortedDictionary<byte, Batch> _batches = new SortedDictionary<byte, Batch>
        {
            [(byte)Channel.Unreliable] = new Batch(),
            [(byte)Channel.UnreliableUnsequenced] = new Batch(),
        };
        readonly Dictionary<byte, Stream> _streams = new Dictionary<byte, Stream>();
        ushort _fragId;
        readonly Dictionary<ushort, Partial> _partial = new Dictionary<ushort, Partial>();
        readonly Queue<byte[]> _out = new Queue<byte[]>();
        readonly double _idle;
        readonly List<double> _samples = new List<double>();
        double _srtt = double.NaN,
            _rttVar;
        public ushort Tick;
        public long Sent,
            Recv,
            Retransmits,
            Dropped;

        /// <summary><paramref name="idle"/> is the idle timeout of what carries the datagrams: a link
        /// whose oldest reliable packet has gone that long without an acknowledgement is dead.</summary>
        public Link(double idle = IdleTimeout)
        {
            _idle = idle;
            _streams[1] = new Stream { Kind = 1, Ordered = true };
            _streams[2] = new Stream { Kind = 2, Ordered = false };
            _streams[Packet.KindFragment] = new Stream
            {
                Kind = Packet.KindFragment,
                Ordered = false,
            };
        }

        static double Now() =>
            System.Diagnostics.Stopwatch.GetTimestamp()
            * 1000.0
            / System.Diagnostics.Stopwatch.Frequency;

        double Rto() =>
            double.IsNaN(_srtt) ? 250 : Math.Min(MaxRto, Math.Max(MinRto, _srtt + 4 * _rttVar));

        public bool IsDead { get; private set; }

        /// <summary>Bytes of reliable messages waiting for the window.</summary>
        public int Backlog()
        {
            int bytes = 0;
            foreach (var stream in _streams.Values)
            {
                bytes += stream.PendingBytes;
            }

            return bytes;
        }

        /// <summary>Ends the link: nothing more is sent or queued, and what was queued is dropped.</summary>
        public void Close()
        {
            IsDead = true;
            foreach (var stream in _streams.Values)
            {
                stream.InFlight.Clear();
                stream.Pending.Clear();
                stream.PendingBytes = 0;
            }
            foreach (var batch in _batches.Values)
            {
                batch.Take();
            }
            _out.Clear();
        }

        Packet.Header Header(byte kind, ushort seq, bool ackOnly = false, bool batch = false)
        {
            _streams.TryGetValue(kind, out var stream);
            bool hasAck = stream != null && stream.HasAck;
            return new Packet.Header
            {
                Kind = kind,
                AckOnly = ackOnly,
                HasAck = hasAck,
                Batch = batch,
                Seq = seq,
                Ack = hasAck ? stream.RxAck : (ushort)0,
                AckBits = hasAck ? stream.RxAckBits : 0,
                Tick = Tick,
            };
        }

        void Push(in Packet.Header header, ReadOnlySpan<byte> payload)
        {
            _out.Enqueue(Packet.Encode(header, payload));
            Sent++;
        }

        public void Send(Channel channel, byte[] msg) => Send(channel, msg, Now());

        /// <summary>Queues a message. Throws on a dead link, and ends the link when the message
        /// would not fit in <see cref="ReliableBacklogBytes"/>.</summary>
        public void Send(Channel channel, byte[] msg, double now)
        {
            if (IsDead)
            {
                throw new InvalidOperationException("peer stopped acknowledging; link dead");
            }

            if (msg.Length > MaxMessage)
            {
                throw new ArgumentException("message too large (max 64 KiB)");
            }

            bool queued =
                msg.Length > Packet.MaxUnfragmented
                || channel == Channel.Reliable
                || channel == Channel.ReliableUnordered;
            if (queued && Backlog() + msg.Length > ReliableBacklogBytes)
            {
                Close();
                throw new InvalidOperationException("reliable messages outran the peer; link dead");
            }

            if (msg.Length > Packet.MaxUnfragmented)
            {
                int count = (msg.Length + FragPayload - 1) / FragPayload;
                if (count > 255)
                {
                    throw new ArgumentException("message too large");
                }

                ushort id = _fragId++;
                for (int i = 0; i < count; i++)
                {
                    int off = i * FragPayload,
                        len = Math.Min(msg.Length - off, FragPayload);
                    var payload = new byte[FragHdr + len];
                    payload[0] = (byte)(id >> 8);
                    payload[1] = (byte)id;
                    payload[2] = (byte)channel;
                    payload[3] = (byte)i;
                    payload[4] = (byte)count;
                    Array.Copy(msg, off, payload, FragHdr, len);
                    Queue(_streams[Packet.KindFragment], payload);
                }
                Pump(now);
                return;
            }
            if (_batches.TryGetValue((byte)channel, out var batch))
            {
                if (!batch.Fits(msg))
                {
                    FlushBatch();
                }

                batch.Add(msg);
                return;
            }
            Queue(_streams[(byte)channel], msg);
            Pump(now);
        }

        static void Queue(Stream stream, byte[] packet)
        {
            stream.Pending.Enqueue(packet);
            stream.PendingBytes += packet.Length;
        }

        public void FlushBatch()
        {
            foreach (var entry in _batches)
            {
                var payload = entry.Value.Take();
                if (payload != null)
                {
                    Push(Header(entry.Key, entry.Value.Seq++, false, true), payload);
                }
            }
        }

        void Pump(double now)
        {
            foreach (var stream in _streams.Values)
            {
                // Bound the span of unacked seqs, not their count: the peer can only ack 32 seqs behind the newest it has seen.
                while (
                    stream.Pending.Count > 0
                    && (ushort)(
                        stream.TxSeq
                        - (stream.InFlight.Count > 0 ? stream.InFlight[0].Seq : stream.TxSeq)
                    ) < Window
                )
                {
                    var payload = stream.Pending.Dequeue();
                    stream.PendingBytes -= payload.Length;
                    ushort seq = stream.TxSeq++;
                    stream.InFlight.Add(
                        new InFlight
                        {
                            Seq = seq,
                            Payload = payload,
                            SentAt = now,
                            FirstSent = now,
                        }
                    );
                    Push(Header(stream.Kind, seq), payload);
                }
            }
        }

        /// <summary>Timer (call every few ms): retransmits and flushes the batch. False when the link is dead.</summary>
        public bool Poll() => Poll(Now());

        public bool Poll(double now)
        {
            if (IsDead)
            {
                return false;
            }

            FlushBatch();
            double waitingSince = double.PositiveInfinity;
            foreach (var stream in _streams.Values)
            {
                if (stream.InFlight.Count > 0)
                {
                    waitingSince = Math.Min(waitingSince, stream.InFlight[0].FirstSent);
                }
            }
            if (now - waitingSince >= _idle)
            {
                Close();
                return false;
            }

            double rto = Rto();
            foreach (var stream in _streams.Values)
            {
                foreach (var packet in stream.InFlight)
                {
                    if (now - packet.SentAt >= rto)
                    {
                        packet.Resent = true;
                        packet.SentAt = now;
                        Retransmits++;
                        Push(Header(stream.Kind, packet.Seq), packet.Payload);
                    }
                }
            }
            if (_partial.Count > 0)
            {
                var stale = new List<ushort>();
                foreach (var kv in _partial)
                {
                    if (now - kv.Value.Started > 10000)
                    {
                        stale.Add(kv.Key);
                    }
                }

                foreach (var id in stale)
                {
                    _partial.Remove(id);
                }
            }
            Pump(now);
            return true;
        }

        public bool TryDequeueOutgoing(out byte[] data)
        {
            if (_out.Count > 0)
            {
                data = _out.Dequeue();
                return true;
            }
            data = null;
            return false;
        }

        bool Reassemble(ArraySegment<byte> frag, double now, ushort tick, out Received received)
        {
            received = default;
            if (frag.Count < FragHdr)
            {
                return false;
            }

            var bytes = frag.Array;
            int offset = frag.Offset;
            ushort id = (ushort)((bytes[offset] << 8) | bytes[offset + 1]);
            var channel = (Channel)bytes[offset + 2];
            int index = bytes[offset + 3],
                count = bytes[offset + 4];
            if (count == 0 || index >= count)
            {
                return false;
            }

            if (!_partial.TryGetValue(id, out var partial))
            {
                if (_partial.Count >= 64)
                {
                    return false;
                }

                partial = new Partial
                {
                    Channel = channel,
                    Parts = new byte[count][],
                    Started = now,
                };
                _partial[id] = partial;
            }
            if (partial.Parts.Length != count || partial.Channel != channel)
            {
                _partial.Remove(id);
                return false;
            }
            if (partial.Parts[index] == null)
            {
                var chunk = new byte[frag.Count - FragHdr];
                Array.Copy(bytes, offset + FragHdr, chunk, 0, chunk.Length);
                partial.Parts[index] = chunk;
                partial.Got++;
            }
            if (partial.Got < count)
            {
                return false;
            }

            _partial.Remove(id);
            int total = 0;
            foreach (var part in partial.Parts)
            {
                total += part.Length;
            }

            var msg = new byte[total];
            int written = 0;
            foreach (var part in partial.Parts)
            {
                Array.Copy(part, 0, msg, written, part.Length);
                written += part.Length;
            }
            received = new Received
            {
                Channel = channel,
                Tick = tick,
                Payload = msg,
            };
            return true;
        }

        public List<Received> Receive(byte[] data) => Receive(data, Now());

        public List<Received> Receive(byte[] data, double now)
        {
            var outp = new List<Received>();
            if (!Packet.Decode(data, out var header, out var payload))
            {
                return outp;
            }

            Recv++;
            if (_batches.ContainsKey(header.Kind))
            {
                var channel = (Channel)header.Kind;
                bool sequenced = channel == Channel.Unreliable;
                if (!sequenced || !_hasSeqRx || Packet.SeqNewer(header.Seq, _seqRx))
                {
                    if (sequenced)
                    {
                        _hasSeqRx = true;
                        _seqRx = header.Seq;
                    }

                    if (header.Batch)
                    {
                        int offset = payload.Offset,
                            end = payload.Offset + payload.Count;
                        var bytes = payload.Array;
                        while (offset + 2 <= end)
                        {
                            int count = (bytes[offset] << 8) | bytes[offset + 1];
                            offset += 2;
                            if (offset + count > end)
                            {
                                break;
                            }

                            var message = new byte[count];
                            Array.Copy(bytes, offset, message, 0, count);
                            offset += count;
                            outp.Add(
                                new Received
                                {
                                    Channel = channel,
                                    Tick = header.Tick,
                                    Payload = message,
                                }
                            );
                        }
                    }
                    else
                    {
                        var message = new byte[payload.Count];
                        Array.Copy(payload.Array, payload.Offset, message, 0, message.Length);
                        outp.Add(
                            new Received
                            {
                                Channel = channel,
                                Tick = header.Tick,
                                Payload = message,
                            }
                        );
                    }
                }
                else
                {
                    Dropped++;
                }

                return outp;
            }
            if (!_streams.TryGetValue(header.Kind, out var stream))
            {
                return outp;
            }

            if (header.HasAck)
            {
                _samples.Clear();
                stream.OnAck(header.Ack, header.AckBits, now, _samples);
                foreach (double sample in _samples)
                {
                    if (double.IsNaN(_srtt))
                    {
                        _srtt = sample;
                        _rttVar = sample / 2;
                    }
                    else
                    {
                        _rttVar = 0.75 * _rttVar + 0.25 * Math.Abs(_srtt - sample);
                        _srtt = 0.875 * _srtt + 0.125 * sample;
                    }
                }
                Pump(now);
            }
            if (!header.AckOnly)
            {
                if (stream.Mark(header.Seq))
                {
                    var copy = new byte[payload.Count];
                    Array.Copy(payload.Array, payload.Offset, copy, 0, copy.Length);
                    if (stream.Ordered)
                    {
                        stream.Buffer[header.Seq] = copy;
                        while (stream.Buffer.TryGetValue(stream.Next, out var message))
                        {
                            Deliver(stream, message, header.Tick, now, outp);
                            stream.Buffer.Remove(stream.Next);
                            stream.Next++;
                        }
                    }
                    else
                    {
                        Deliver(stream, copy, header.Tick, now, outp);
                    }
                }
                Push(Header(stream.Kind, stream.TxSeq, true), ReadOnlySpan<byte>.Empty);
            }
            return outp;
        }

        void Deliver(Stream stream, byte[] payload, ushort tick, double now, List<Received> outp)
        {
            if (stream.Kind == Packet.KindFragment)
            {
                if (Reassemble(new ArraySegment<byte>(payload), now, tick, out var received))
                {
                    outp.Add(received);
                }
            }
            else
            {
                outp.Add(
                    new Received
                    {
                        Channel = (Channel)stream.Kind,
                        Tick = tick,
                        Payload = payload,
                    }
                );
            }
        }
    }
}
