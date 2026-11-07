// MSB-first bit packing, mirrors net-wire::bits.
using System;
using System.Collections.Generic;

namespace Aurix.Net.Managed
{
    public sealed class BitWriter
    {
        readonly List<byte> _buf = new List<byte>(); ulong _scratch; int _n; public int BitLen { get; private set; }
        public void WriteBits(ulong v, int nbits)
        {
            if (nbits == 0) return;
            if (nbits < 64) v &= (1UL << nbits) - 1;
            BitLen += nbits;
            while (nbits > 0)
            {
                int take = Math.Min(nbits, 64 - _n);
                // append the top `take` bits of v's remaining nbits
                ulong part = nbits == 64 && take == 64 ? v : (v >> (nbits - take)) & (take == 64 ? ulong.MaxValue : (1UL << take) - 1);
                if (take == 64) { _scratch = part; _n = 64; }
                else { _scratch = (_scratch << take) | part; _n += take; }
                nbits -= take;
                while (_n >= 8) { _buf.Add((byte)(_scratch >> (_n - 8))); _n -= 8; }
            }
        }
        public void WriteBool(bool b) => WriteBits(b ? 1UL : 0UL, 1);
        public void WriteUInt(ulong v, int nbits) { ulong max = nbits >= 64 ? ulong.MaxValue : (1UL << nbits) - 1; WriteBits(Math.Min(v, max), nbits); }
        public void WriteInt(long v, int nbits) => WriteUInt((ulong)((v << 1) ^ (v >> 63)), nbits);
        public void WriteQuantized(float v, float min, float max, float res) { Bits.QuantBits(min, max, res, out int nb, out _); ulong q = (ulong)(((Math.Min(Math.Max(v, min), max) - min) / res) + 0.5f); WriteUInt(q, nb); }
        public void WriteF32(float v) => WriteBits(BitConverter.ToUInt32(BitConverter.GetBytes(v), 0), 32);
        public void WriteBytes(byte[] b) { foreach (var x in b) WriteBits(x, 8); }
        public byte[] Finish() { var o = new List<byte>(_buf); if (_n > 0) o.Add((byte)(_scratch << (8 - _n))); return o.ToArray(); }
    }

    public sealed class BitReader
    {
        readonly byte[] _b; readonly int _off; readonly int _bits; int _pos;
        public BitReader(byte[] b, int offset = 0) { _b = b; _off = offset; _bits = (b.Length - offset) * 8; }
        public int BitsLeft => _bits - _pos;
        public ulong ReadBits(int nbits)
        {
            if (nbits > BitsLeft) throw new NetException("delta truncated");
            ulong v = 0;
            for (int i = 0; i < nbits; i++) { int p = _pos + i; v = (v << 1) | (ulong)((_b[_off + (p >> 3)] >> (7 - (p & 7))) & 1); }
            _pos += nbits; return v;
        }
        public bool ReadBool() => ReadBits(1) == 1;
        public ulong ReadUInt(int nbits) => ReadBits(nbits);
        public long ReadInt(int nbits) { ulong zz = ReadBits(nbits); return (long)(zz >> 1) ^ -(long)(zz & 1); }
        public float ReadQuantized(float min, float max, float res) { Bits.QuantBits(min, max, res, out int nb, out _); return min + ReadUInt(nb) * res; }
        public float ReadF32() => BitConverter.ToSingle(BitConverter.GetBytes((uint)ReadBits(32)), 0);
        public byte[] ReadBytes(int n) { var o = new byte[n]; for (int i = 0; i < n; i++) o[i] = (byte)ReadBits(8); return o; }
    }

    public static class Bits
    {
        public static void QuantBits(float min, float max, float res, out int bits, out ulong steps)
        {
            steps = Math.Max(1UL, (ulong)Math.Ceiling((max - min) / res));
            bits = 64 - LeadingZeros(steps); if (bits < 1) bits = 1;
        }
        public static int LeadingZeros(ulong v) { int n = 0; for (int i = 63; i >= 0; i--) { if (((v >> i) & 1) != 0) break; n++; } return n; }
        public static int LenBits(uint max) => Math.Max(1, 64 - LeadingZeros(max));
    }
}
