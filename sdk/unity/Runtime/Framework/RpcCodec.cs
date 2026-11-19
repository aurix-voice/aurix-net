// Optional RPC payload packing: delta against the last call per peer, Deflate compression.
// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Compression;

namespace Aurix.Net.Unity.Framework
{
    public enum RpcCompression : byte
    {
        None,
        Fast,
        Balanced,
        Best,
    }

    /// <summary>
    /// Packs RPC argument payloads when the attribute asks for it. Delta packing XORs the payload with the previous
    /// call of the same RPC to the same peer and run-length encodes the zeros (great for state-like RPCs that change a
    /// little each call); compression runs Deflate when it pays off. Both sides keep per-(RPC, peer) history; the first
    /// call to a peer is always sent whole so a receiver never lacks a base.
    /// </summary>
    public sealed class RpcCodec
    {
        const byte FlagCompressed = 1,
            FlagDelta = 2;
        const int MinCompressBytes = 48;
        readonly Dictionary<(ulong key, int peer), byte[]> _sent = new Dictionary<
                (ulong, int),
                byte[]
            >(),
            _recv = new Dictionary<(ulong, int), byte[]>();

        public static bool Wants(RpcAttribute attribute) =>
            attribute.compression != RpcCompression.None || attribute.deltaPack;

        public void Clear()
        {
            _sent.Clear();
            _recv.Clear();
        }

        public void ForgetPeer(int peer)
        {
            var ks = new List<(ulong, int)>();
            foreach (var key in _sent.Keys)
            {
                if (key.peer == peer)
                {
                    ks.Add(key);
                }
            }

            foreach (var key in ks)
            {
                _sent.Remove(key);
            }

            ks.Clear();
            foreach (var key in _recv.Keys)
            {
                if (key.peer == peer)
                {
                    ks.Add(key);
                }
            }

            foreach (var key in ks)
            {
                _recv.Remove(key);
            }
        }

        public byte[] Encode(ulong key, int peer, RpcAttribute attribute, byte[] raw)
        {
            if (!Wants(attribute))
            {
                return raw;
            }

            byte flags = 0;
            var body = raw;
            if (attribute.deltaPack)
            {
                if (_sent.TryGetValue((key, peer), out var prev) && prev.Length == raw.Length)
                {
                    body = RleZeros(Xor(raw, prev));
                    flags |= FlagDelta;
                }
                _sent[(key, peer)] = raw;
            }
            if (attribute.compression != RpcCompression.None && body.Length >= MinCompressBytes)
            {
                var compressed = Deflate(body, attribute.compression);
                if (compressed.Length < body.Length)
                {
                    body = compressed;
                    flags |= FlagCompressed;
                }
            }
            var output = new byte[body.Length + 1];
            output[0] = flags;
            Buffer.BlockCopy(body, 0, output, 1, body.Length);
            return output;
        }

        public byte[] Decode(ulong key, int peer, RpcAttribute attribute, byte[] wire)
        {
            if (!Wants(attribute))
            {
                return wire;
            }

            if (wire.Length == 0)
            {
                return wire;
            }

            byte flags = wire[0];
            var body = new byte[wire.Length - 1];
            Buffer.BlockCopy(wire, 1, body, 0, body.Length);
            if ((flags & FlagCompressed) != 0)
            {
                body = Inflate(body);
            }

            if ((flags & FlagDelta) != 0)
            {
                if (!_recv.TryGetValue((key, peer), out var prev))
                {
                    throw new NetException("RpcCodec: delta-packed RPC without a base payload");
                }

                body = Xor(UnRleZeros(body, prev.Length), prev);
            }
            if (attribute.deltaPack)
            {
                _recv[(key, peer)] = body;
            }

            return body;
        }

        static byte[] Xor(byte[] left, byte[] right)
        {
            var output = new byte[left.Length];
            for (int i = 0; i < left.Length; i++)
            {
                output[i] = (byte)(left[i] ^ right[i]);
            }

            return output;
        }

        // runs: [zeroRun:1][litLen:1][lit...]... ; runs longer than 255 split.
        static byte[] RleZeros(byte[] data)
        {
            var output = new List<byte>(data.Length / 2 + 4);
            int index = 0;
            while (index < data.Length)
            {
                int zeros = 0;
                while (index < data.Length && data[index] == 0 && zeros < 255)
                {
                    zeros++;
                    index++;
                }
                int start = index;
                while (index < data.Length && data[index] != 0 && index - start < 255)
                {
                    index++;
                }

                output.Add((byte)zeros);
                output.Add((byte)(index - start));
                for (int k = start; k < index; k++)
                {
                    output.Add(data[k]);
                }
            }
            return output.ToArray();
        }

        static byte[] UnRleZeros(byte[] encoded, int length)
        {
            var output = new byte[length];
            int index = 0,
                position = 0;
            while (index + 1 < encoded.Length && position < length)
            {
                int zeros = encoded[index++],
                    literals = encoded[index++];
                position += zeros;
                for (int k = 0; k < literals && position < length; k++)
                {
                    output[position++] = encoded[index++];
                }
            }
            return output;
        }

        static byte[] Deflate(byte[] data, RpcCompression level)
        {
            var lv =
                level == RpcCompression.Fast ? CompressionLevel.Fastest : CompressionLevel.Optimal;
            using (var ms = new MemoryStream())
            {
                using (var deflate = new DeflateStream(ms, lv, true))
                {
                    deflate.Write(data, 0, data.Length);
                }

                return ms.ToArray();
            }
        }

        static byte[] Inflate(byte[] data)
        {
            using (var src = new MemoryStream(data))
            using (var deflate = new DeflateStream(src, CompressionMode.Decompress))
            using (var ms = new MemoryStream())
            {
                deflate.CopyTo(ms);
                return ms.ToArray();
            }
        }
    }
}
