// MSB-first bit packing, mirrors net-wire::bits.
using System;
using System.Collections.Generic;

namespace Aurix.Net.Managed
{
    public sealed class BitWriter
    {
        readonly List<byte> _buf = new List<byte>();
        ulong _scratch;
        int _n;
        public int BitLen { get; private set; }

        public void WriteBits(ulong value, int nbits)
        {
            if (nbits == 0)
            {
                return;
            }

            if (nbits < 64)
            {
                value &= (1UL << nbits) - 1;
            }

            BitLen += nbits;
            while (nbits > 0)
            {
                int take = Math.Min(nbits, 64 - _n);
                // append the top `take` bits of v's remaining nbits
                ulong part =
                    nbits == 64 && take == 64
                        ? value
                        : (value >> (nbits - take))
                            & (take == 64 ? ulong.MaxValue : (1UL << take) - 1);
                if (take == 64)
                {
                    _scratch = part;
                    _n = 64;
                }
                else
                {
                    _scratch = (_scratch << take) | part;
                    _n += take;
                }
                nbits -= take;
                while (_n >= 8)
                {
                    _buf.Add((byte)(_scratch >> (_n - 8)));
                    _n -= 8;
                }
            }
        }

        public void WriteBool(bool value) => WriteBits(value ? 1UL : 0UL, 1);

        public void WriteUInt(ulong value, int nbits)
        {
            ulong max = nbits >= 64 ? ulong.MaxValue : (1UL << nbits) - 1;
            WriteBits(Math.Min(value, max), nbits);
        }

        /// <summary>Signed integer in [-2^(nbits-1), 2^(nbits-1)), clamped, via zigzag.</summary>
        public void WriteInt(long value, int nbits)
        {
            if (nbits < 64)
            {
                long half = 1L << (nbits - 1);
                value = Math.Min(Math.Max(value, -half), half - 1);
            }

            WriteBits((ulong)((value << 1) ^ (value >> 63)), nbits);
        }

        public void WriteQuantized(float value, float min, float max, float res)
        {
            Bits.QuantBits(min, max, res, out int nb, out _);
            ulong quantized = (ulong)(((Math.Min(Math.Max(value, min), max) - min) / res) + 0.5f);
            WriteUInt(quantized, nb);
        }

        public void WriteF32(float value) =>
            WriteBits(BitConverter.ToUInt32(BitConverter.GetBytes(value), 0), 32);

        public void WriteBytes(byte[] bytes)
        {
            foreach (var value in bytes)
            {
                WriteBits(value, 8);
            }
        }

        public byte[] Finish()
        {
            var bytes = new List<byte>(_buf);
            if (_n > 0)
            {
                bytes.Add((byte)(_scratch << (8 - _n)));
            }

            return bytes.ToArray();
        }
    }

    public sealed class BitReader
    {
        readonly byte[] _b;
        readonly int _off;
        readonly int _bits;
        int _pos;

        public BitReader(byte[] bytes, int offset = 0)
        {
            _b = bytes;
            _off = offset;
            _bits = (bytes.Length - offset) * 8;
        }

        public int BitsLeft => _bits - _pos;

        public ulong ReadBits(int nbits)
        {
            if (nbits > BitsLeft)
            {
                throw new NetException("delta truncated");
            }

            ulong value = 0;
            for (int i = 0; i < nbits; i++)
            {
                int position = _pos + i;
                value =
                    (value << 1) | (uint)((_b[_off + (position >> 3)] >> (7 - (position & 7))) & 1);
            }
            _pos += nbits;
            return value;
        }

        public bool ReadBool() => ReadBits(1) == 1;

        public ulong ReadUInt(int nbits) => ReadBits(nbits);

        public long ReadInt(int nbits)
        {
            ulong zz = ReadBits(nbits);
            return (long)(zz >> 1) ^ -(long)(zz & 1);
        }

        public float ReadQuantized(float min, float max, float res)
        {
            Bits.QuantBits(min, max, res, out int nb, out _);
            return min + ReadUInt(nb) * res;
        }

        public float ReadF32() =>
            BitConverter.ToSingle(BitConverter.GetBytes((uint)ReadBits(32)), 0);

        public byte[] ReadBytes(int count)
        {
            var output = new byte[count];
            for (int i = 0; i < count; i++)
            {
                output[i] = (byte)ReadBits(8);
            }

            return output;
        }
    }

    public static class Bits
    {
        public static void QuantBits(float min, float max, float res, out int bits, out ulong steps)
        {
            steps = Math.Max(1UL, (ulong)Math.Ceiling((max - min) / res));
            bits = 64 - LeadingZeros(steps);
            if (bits < 1)
            {
                bits = 1;
            }
        }

        public static int LeadingZeros(ulong value)
        {
            int count = 0;
            for (int i = 63; i >= 0; i--)
            {
                if (((value >> i) & 1) != 0)
                {
                    break;
                }

                count++;
            }
            return count;
        }

        public static int LenBits(uint max) => Math.Max(1, 64 - LeadingZeros(max));
    }
}
