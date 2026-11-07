// Client side of lockstep rooms: confirmed input bundles in tick order, late-join catch-up
// (checkpoint + journal tail), hash reports, checkpoint uploads when the node asks the master.
// The game owns the deterministic simulation: step it in OnTick, hash it, restore it in OnRestore.
using System;
using System.Collections.Generic;

namespace Aurix.Net.Managed
{
    public sealed class LockstepClient
    {
        public const byte KindInput = 0x10, KindBundle = 0x20, KindHash = 0x21, KindCheckpoint = 0x22, KindCheckpointReq = 0x23, KindCatchup = 0x24;

        /// <summary>Confirmed inputs for one tick, in tick order, every tick exactly once (empty dictionary = nobody sent).</summary>
        public event Action<uint, Dictionary<ushort, byte[]>> OnTick;
        /// <summary>Catch-up start: restore the game state from this checkpoint (state empty at tick 0 = fresh start).</summary>
        public event Action<uint, ulong, byte[]> OnRestore;
        /// <summary>The node asked us (the master) for the confirmed state at a tick ≤ this one: answer with SendCheckpoint.</summary>
        public event Action<uint> OnCheckpointRequested;

        readonly Action<Channel, byte[]> _send;
        readonly SortedDictionary<uint, Dictionary<ushort, byte[]>> _pending = new SortedDictionary<uint, Dictionary<ushort, byte[]>>();
        readonly List<byte[]> _recentInputs = new List<byte[]>();
        bool _started; uint _next; uint _lastInput;
        /// <summary>Next tick to be delivered to OnTick.</summary>
        public uint ConfirmedTick => _next;
        /// <summary>Newest tick the node has bundled (+1).</summary>
        public uint ServerTick { get; private set; }
        /// <summary>How many ticks ahead of the server clock inputs are stamped (≥1).</summary>
        public uint InputDelay = 2;
        public long Delivered, Catchups, Dropped;

        public LockstepClient(Action<Channel, byte[]> send) { _send = send; }

        /// <summary>Feed every data-path payload from a lockstep room. Returns false when it was not a lockstep frame.</summary>
        public bool Receive(byte[] p)
        {
            if (p.Length == 0) return false;
            switch (p[0])
            {
                case KindCatchup:
                    {
                        if (p.Length < 17) return true;
                        uint tick = U32(p, 1); ulong hash = U64(p, 5); int len = (int)U32(p, 13);
                        var state = new byte[Math.Min(len, p.Length - 17)]; Array.Copy(p, 17, state, 0, state.Length);
                        _pending.Clear(); _next = tick; _started = true; Catchups++;
                        OnRestore?.Invoke(tick, hash, state);
                        return true;
                    }
                case KindBundle:
                    {
                        if (p.Length < 2) return true;
                        int n = p[1], o = 2;
                        for (int i = 0; i < n && o + 2 <= p.Length; i++)
                        {
                            int len = (p[o] << 8) | p[o + 1]; o += 2; if (o + len > p.Length) break;
                            DecodeBundle(p, o, len); o += len;
                        }
                        Drain();
                        return true;
                    }
                case KindCheckpointReq:
                    if (p.Length >= 5) OnCheckpointRequested?.Invoke(U32(p, 1));
                    return true;
                case KindCheckpoint: case KindHash: case KindInput:
                    return true;
            }
            return false;
        }

        void DecodeBundle(byte[] b, int o, int len)
        {
            int end = o + len; if (len < 6 || b[o] != KindBundle) return;
            uint t = U32(b, o + 1); int k = b[o + 5]; o += 6;
            if (t + 1 > ServerTick) ServerTick = t + 1;
            if (!_started) { _started = true; _next = t; }
            if (t < _next || _pending.ContainsKey(t)) return;
            var inputs = new Dictionary<ushort, byte[]>(k);
            for (int i = 0; i < k && o + 3 <= end; i++)
            {
                ushort a = (ushort)((b[o] << 8) | b[o + 1]); int l = b[o + 2]; o += 3; if (o + l > end) break;
                var d = new byte[l]; Array.Copy(b, o, d, 0, l); o += l; inputs[a] = d;
            }
            _pending[t] = inputs;
        }

        void Drain()
        {
            while (_pending.TryGetValue(_next, out var inputs))
            {
                _pending.Remove(_next); var t = _next; _next++; Delivered++;
                OnTick?.Invoke(t, inputs);
            }
            // Ticks older than the redundancy window that never arrived are lost for good only if the
            // node skipped them; the node re-sends 3 ticks per datagram and the journal tail is reliable.
            while (_pending.Count > 0)
            {
                var e = _pending.GetEnumerator(); e.MoveNext(); var oldest = e.Current.Key;
                if (oldest <= _next + 600) break;
                _pending.Remove(oldest); Dropped++;
            }
        }

        /// <summary>Queues this tick's local input (sent with the previous two for redundancy).</summary>
        public void SendInput(byte[] input)
        {
            uint tick = Math.Max(ServerTick + InputDelay, _lastInput + 1); _lastInput = tick;
            _recentInputs.Add(input); while (_recentInputs.Count > 3) _recentInputs.RemoveAt(0);
            _send(Channel.Unreliable, SyncWorld.EncodeInput(tick - (uint)(_recentInputs.Count - 1), _recentInputs));
        }

        public void ReportHash(uint tick, ulong hash)
        {
            var b = new byte[13]; b[0] = KindHash; W32(b, 1, tick); W64(b, 5, hash); _send(Channel.Unreliable, b);
        }

        /// <summary>Master only, in answer to OnCheckpointRequested: confirmed state at `tick` (≤ requested).</summary>
        public void SendCheckpoint(uint tick, ulong hash, byte[] state)
        {
            var b = new byte[13 + state.Length]; b[0] = KindCheckpoint; W32(b, 1, tick); W64(b, 5, hash); Array.Copy(state, 0, b, 13, state.Length);
            _send(Channel.Reliable, b);
        }

        /// <summary>FNV-1a 64 over bytes: a simple, portable state hash.</summary>
        public static ulong Fnv1a(byte[] data) { ulong h = 14695981039346656037UL; foreach (var x in data) { h ^= x; h *= 1099511628211UL; } return h; }

        static uint U32(byte[] b, int o) => ((uint)b[o] << 24) | ((uint)b[o + 1] << 16) | ((uint)b[o + 2] << 8) | b[o + 3];
        static ulong U64(byte[] b, int o) => ((ulong)U32(b, o) << 32) | U32(b, o + 4);
        static void W32(byte[] b, int o, uint v) { b[o] = (byte)(v >> 24); b[o + 1] = (byte)(v >> 16); b[o + 2] = (byte)(v >> 8); b[o + 3] = (byte)v; }
        static void W64(byte[] b, int o, ulong v) { W32(b, o, (uint)(v >> 32)); W32(b, o + 4, (uint)v); }
    }
}
