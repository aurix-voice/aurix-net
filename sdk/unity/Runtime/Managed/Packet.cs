// Aurix Net wire format, pure C# (mirrors net-transport::packet / sdk/web/src/packet.ts).
// 12-byte big-endian header: u8 version, u8 kind|flags, u16 seq, u16 ack, u32 ack_bits, u16 tick.
using System;

namespace Aurix.Net.Managed
{
    public static class Packet
    {
        public const byte ProtocolVersion = 1;
        public const int HeaderLen = 12;
        public const int MaxDatagram = 1100;
        public const int MaxUnfragmented = MaxDatagram - HeaderLen;
        public const byte FlagAckOnly = 0x80, FlagHasAck = 0x40, FlagBatch = 0x20;
        public const byte KindFragment = 3;

        public struct Header
        {
            public byte Kind; public bool AckOnly, HasAck, Batch;
            public ushort Seq, Ack; public uint AckBits; public ushort Tick;
        }

        public static byte[] Encode(in Header h, ReadOnlySpan<byte> payload)
        {
            var o = new byte[HeaderLen + payload.Length];
            o[0] = ProtocolVersion;
            o[1] = (byte)((h.Kind & 0x1f) | (h.AckOnly ? FlagAckOnly : 0) | (h.HasAck ? FlagHasAck : 0) | (h.Batch ? FlagBatch : 0));
            o[2] = (byte)(h.Seq >> 8); o[3] = (byte)h.Seq; o[4] = (byte)(h.Ack >> 8); o[5] = (byte)h.Ack;
            o[6] = (byte)(h.AckBits >> 24); o[7] = (byte)(h.AckBits >> 16); o[8] = (byte)(h.AckBits >> 8); o[9] = (byte)h.AckBits;
            o[10] = (byte)(h.Tick >> 8); o[11] = (byte)h.Tick;
            payload.CopyTo(o.AsSpan(HeaderLen));
            return o;
        }

        public static bool Decode(byte[] d, out Header h, out ArraySegment<byte> payload)
        {
            h = default; payload = default;
            if (d.Length < HeaderLen || d[0] != ProtocolVersion) return false;
            byte k = d[1];
            h.Kind = (byte)(k & 0x1f); h.AckOnly = (k & FlagAckOnly) != 0; h.HasAck = (k & FlagHasAck) != 0; h.Batch = (k & FlagBatch) != 0;
            h.Seq = (ushort)((d[2] << 8) | d[3]); h.Ack = (ushort)((d[4] << 8) | d[5]);
            h.AckBits = ((uint)d[6] << 24) | ((uint)d[7] << 16) | ((uint)d[8] << 8) | d[9];
            h.Tick = (ushort)((d[10] << 8) | d[11]);
            payload = new ArraySegment<byte>(d, HeaderLen, d.Length - HeaderLen);
            return true;
        }

        /// <summary>true when a is a newer 16-bit sequence than b.</summary>
        public static bool SeqNewer(ushort a, ushort b) => a != b && (ushort)(a - b) < 0x8000;
    }
}
