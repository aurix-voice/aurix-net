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
        public const byte FlagAckOnly = 0x80,
            FlagHasAck = 0x40,
            FlagBatch = 0x20;
        public const byte KindFragment = 3;

        public struct Header
        {
            public byte Kind;
            public bool AckOnly,
                HasAck,
                Batch;
            public ushort Seq,
                Ack;
            public uint AckBits;
            public ushort Tick;
        }

        public static byte[] Encode(in Header header, ReadOnlySpan<byte> payload)
        {
            var output = new byte[HeaderLen + payload.Length];
            output[0] = ProtocolVersion;
            output[1] = (byte)(
                (header.Kind & 0x1f)
                | (header.AckOnly ? FlagAckOnly : 0)
                | (header.HasAck ? FlagHasAck : 0)
                | (header.Batch ? FlagBatch : 0)
            );
            output[2] = (byte)(header.Seq >> 8);
            output[3] = (byte)header.Seq;
            output[4] = (byte)(header.Ack >> 8);
            output[5] = (byte)header.Ack;
            output[6] = (byte)(header.AckBits >> 24);
            output[7] = (byte)(header.AckBits >> 16);
            output[8] = (byte)(header.AckBits >> 8);
            output[9] = (byte)header.AckBits;
            output[10] = (byte)(header.Tick >> 8);
            output[11] = (byte)header.Tick;
            payload.CopyTo(output.AsSpan(HeaderLen));
            return output;
        }

        public static bool Decode(byte[] data, out Header header, out ArraySegment<byte> payload)
        {
            header = default;
            payload = default;
            if (data.Length < HeaderLen || data[0] != ProtocolVersion)
            {
                return false;
            }

            byte kind = data[1];
            header.Kind = (byte)(kind & 0x1f);
            header.AckOnly = (kind & FlagAckOnly) != 0;
            header.HasAck = (kind & FlagHasAck) != 0;
            header.Batch = (kind & FlagBatch) != 0;
            header.Seq = (ushort)((data[2] << 8) | data[3]);
            header.Ack = (ushort)((data[4] << 8) | data[5]);
            header.AckBits =
                ((uint)data[6] << 24) | ((uint)data[7] << 16) | ((uint)data[8] << 8) | data[9];
            header.Tick = (ushort)((data[10] << 8) | data[11]);
            payload = new ArraySegment<byte>(data, HeaderLen, data.Length - HeaderLen);
            return true;
        }

        /// <summary>true when a is a newer 16-bit sequence than b.</summary>
        public static bool SeqNewer(ushort left, ushort right) =>
            left != right && (ushort)(left - right) < 0x8000;
    }
}
