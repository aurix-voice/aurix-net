// Aurix Net Unity SDK — optional RPC payload packing: delta against the last call per peer, Deflate compression.
// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Compression;

namespace Aurix.Net.Unity.Framework
{
    public enum RpcCompression : byte { None, Fast, Balanced, Best }

    /// <summary>
    /// Packs RPC argument payloads when the attribute asks for it. Delta packing XORs the payload with the previous
    /// call of the same RPC to the same peer and run-length encodes the zeros (great for state-like RPCs that change a
    /// little each call); compression runs Deflate when it pays off. Both sides keep per-(RPC, peer) history; the first
    /// call to a peer is always sent whole so a receiver never lacks a base.
    /// </summary>
    public sealed class RpcCodec
    {
        const byte FlagCompressed = 1, FlagDelta = 2;
        const int MinCompressBytes = 48;
        readonly Dictionary<(ulong key, int peer), byte[]> _sent = new Dictionary<(ulong, int), byte[]>(), _recv = new Dictionary<(ulong, int), byte[]>();

        public static bool Wants(RpcAttribute a) => a.compression != RpcCompression.None || a.deltaPack;
        public void Clear() { _sent.Clear(); _recv.Clear(); }
        public void ForgetPeer(int peer)
        {
            var ks = new List<(ulong, int)>(); foreach (var k in _sent.Keys) if (k.peer == peer) ks.Add(k); foreach (var k in ks) _sent.Remove(k);
            ks.Clear(); foreach (var k in _recv.Keys) if (k.peer == peer) ks.Add(k); foreach (var k in ks) _recv.Remove(k);
        }

        public byte[] Encode(ulong key, int peer, RpcAttribute a, byte[] raw)
        {
            if (!Wants(a)) return raw;
            byte flags = 0; var body = raw;
            if (a.deltaPack)
            {
                if (_sent.TryGetValue((key, peer), out var prev) && prev.Length == raw.Length) { body = RleZeros(Xor(raw, prev)); flags |= FlagDelta; }
                _sent[(key, peer)] = raw;
            }
            if (a.compression != RpcCompression.None && body.Length >= MinCompressBytes)
            {
                var c = Deflate(body, a.compression);
                if (c.Length < body.Length) { body = c; flags |= FlagCompressed; }
            }
            var o = new byte[body.Length + 1]; o[0] = flags; Buffer.BlockCopy(body, 0, o, 1, body.Length); return o;
        }

        public byte[] Decode(ulong key, int peer, RpcAttribute a, byte[] wire)
        {
            if (!Wants(a)) return wire;
            if (wire.Length == 0) return wire;
            byte flags = wire[0]; var body = new byte[wire.Length - 1]; Buffer.BlockCopy(wire, 1, body, 0, body.Length);
            if ((flags & FlagCompressed) != 0) body = Inflate(body);
            if ((flags & FlagDelta) != 0)
            {
                if (!_recv.TryGetValue((key, peer), out var prev)) throw new NetException("RpcCodec: delta-packed RPC without a base payload");
                body = Xor(UnRleZeros(body, prev.Length), prev);
            }
            if (a.deltaPack) _recv[(key, peer)] = body;
            return body;
        }

        static byte[] Xor(byte[] a, byte[] b) { var o = new byte[a.Length]; for (int i = 0; i < a.Length; i++) o[i] = (byte)(a[i] ^ b[i]); return o; }

        // runs: [zeroRun:1][litLen:1][lit...]... ; runs longer than 255 split.
        static byte[] RleZeros(byte[] d)
        {
            var o = new List<byte>(d.Length / 2 + 4); int i = 0;
            while (i < d.Length)
            {
                int z = 0; while (i < d.Length && d[i] == 0 && z < 255) { z++; i++; }
                int start = i; while (i < d.Length && d[i] != 0 && i - start < 255) i++;
                o.Add((byte)z); o.Add((byte)(i - start)); for (int k = start; k < i; k++) o.Add(d[k]);
            }
            return o.ToArray();
        }
        static byte[] UnRleZeros(byte[] e, int length)
        {
            var o = new byte[length]; int i = 0, p = 0;
            while (i + 1 < e.Length && p < length)
            {
                int z = e[i++], l = e[i++]; p += z;
                for (int k = 0; k < l && p < length; k++) o[p++] = e[i++];
            }
            return o;
        }

        static byte[] Deflate(byte[] d, RpcCompression level)
        {
            var lv = level == RpcCompression.Fast ? CompressionLevel.Fastest : CompressionLevel.Optimal;
            using (var ms = new MemoryStream()) { using (var z = new DeflateStream(ms, lv, true)) z.Write(d, 0, d.Length); return ms.ToArray(); }
        }
        static byte[] Inflate(byte[] d)
        {
            using (var src = new MemoryStream(d)) using (var z = new DeflateStream(src, CompressionMode.Decompress)) using (var ms = new MemoryStream()) { z.CopyTo(ms); return ms.ToArray(); }
        }
    }
}
