// Client side of lockstep rooms: confirmed input bundles in tick order, late-join catch-up
// (checkpoint + journal tail), hash reports, checkpoint uploads when the node asks the master.
// The game owns the deterministic simulation: step it in OnTick, hash it, restore it in OnRestore.
using System;
using System.Collections.Generic;

namespace Aurix.Net.Managed
{
    public sealed class LockstepClient
    {
        public const byte KindInput = 0x10,
            KindBundle = 0x20,
            KindHash = 0x21,
            KindCheckpoint = 0x22,
            KindCheckpointReq = 0x23,
            KindCatchup = 0x24,
            KindInputStats = 0x25,
            KindDumpReq = 0x26,
            KindDump = 0x27;
        public const byte KindCommand = 0x28;

        /// <summary>Confirmed inputs for one tick, in tick order, every tick exactly once (empty dictionary = nobody sent).</summary>
        public event Action<uint, Dictionary<ushort, byte[]>> OnTick;

        /// <summary>Pseudo-actor whose input carries the tick's commands (see SendCommand).</summary>
        public const ushort CommandActor = 0xFFFF;

        /// <summary>Reliable command delivered to everyone at the same tick: (tick, sender actor, bytes). Raised before OnTick of that tick.</summary>
        public event Action<uint, ushort, byte[]> OnCommand;

        /// <summary>Catch-up start: restore the game state from this checkpoint (state empty at tick 0 = fresh start).</summary>
        public event Action<uint, ulong, byte[]> OnRestore;

        /// <summary>The node asked us (the master) for the confirmed state at a tick ≤ this one: answer with SendCheckpoint.</summary>
        public event Action<uint> OnCheckpointRequested;

        /// <summary>The node flagged a desync and wants our state at this tick (from your rollback history): answer with SendDump, or ignore.</summary>
        public event Action<uint> OnDumpRequested;

        readonly Action<Channel, byte[]> _send;
        readonly SortedDictionary<uint, Dictionary<ushort, byte[]>> _pending =
            new SortedDictionary<uint, Dictionary<ushort, byte[]>>();
        readonly List<byte[]> _recentInputs = new List<byte[]>();
        bool _started;
        uint _next;
        uint _lastInput;

        /// <summary>Next tick to be delivered to OnTick.</summary>
        public uint ConfirmedTick => _next;

        /// <summary>Newest tick the node has bundled (+1).</summary>
        public uint ServerTick { get; private set; }

        /// <summary>How many ticks ahead of the server clock inputs are stamped (≥1). Tuned automatically between
        /// InputDelayMin and InputDelayMax from the node's input-margin reports when AdaptiveDelay is on.</summary>
        public uint InputDelay = 2;
        public uint InputDelayMin = 1,
            InputDelayMax = 8;
        public bool AdaptiveDelay = true;

        /// <summary>Inputs repeated per datagram (1..8): higher survives more loss at the cost of bytes.</summary>
        public int InputRedundancy = 3;

        /// <summary>Last reported margin (ticks of slack; negative = inputs arrived late) and late inputs so far.</summary>
        public sbyte LastMargin;
        public long LateInputs;
        int _slackStreak;
        public long Delivered,
            Catchups,
            Dropped;

        public LockstepClient(Action<Channel, byte[]> send)
        {
            _send = send;
        }

        /// <summary>Feed every data-path payload from a lockstep room. Returns false when it was not a lockstep frame.</summary>
        public bool Receive(byte[] payload)
        {
            if (payload.Length == 0)
            {
                return false;
            }

            switch (payload[0])
            {
                case KindCatchup:
                {
                    if (payload.Length < 17)
                    {
                        return true;
                    }

                    uint tick = U32(payload, 1);
                    ulong hash = U64(payload, 5);
                    int len = (int)U32(payload, 13);
                    var state = new byte[Math.Min(len, payload.Length - 17)];
                    Array.Copy(payload, 17, state, 0, state.Length);
                    _pending.Clear();
                    _next = tick;
                    _started = true;
                    Catchups++;
                    OnRestore?.Invoke(tick, hash, state);
                    return true;
                }
                case KindBundle:
                {
                    if (payload.Length < 2)
                    {
                        return true;
                    }

                    int count = payload[1],
                        offset = 2;
                    for (int i = 0; i < count && offset + 2 <= payload.Length; i++)
                    {
                        int len = (payload[offset] << 8) | payload[offset + 1];
                        offset += 2;
                        if (offset + len > payload.Length)
                        {
                            break;
                        }

                        DecodeBundle(payload, offset, len);
                        offset += len;
                    }
                    Drain();
                    return true;
                }
                case KindCheckpointReq:
                    if (payload.Length >= 5)
                    {
                        OnCheckpointRequested?.Invoke(U32(payload, 1));
                    }

                    return true;
                case KindDumpReq:
                    if (payload.Length >= 5)
                    {
                        OnDumpRequested?.Invoke(U32(payload, 1));
                    }

                    return true;
                case KindInputStats:
                    if (payload.Length >= 7)
                    {
                        OnInputStats((sbyte)payload[5], payload[6]);
                    }

                    return true;
                case KindCheckpoint:
                case KindHash:
                case KindInput:
                    return true;
            }
            return false;
        }

        /// <summary>Raises the input delay as soon as inputs arrive late, lowers it one tick at a time after three
        /// consecutive reports with two or more ticks of slack.</summary>
        internal void OnInputStats(sbyte margin, byte late)
        {
            LastMargin = margin;
            LateInputs += late;
            if (!AdaptiveDelay)
            {
                return;
            }

            if (margin < 0)
            {
                InputDelay = Math.Min(InputDelayMax, InputDelay + (uint)Math.Min(4, -margin));
                _slackStreak = 0;
            }
            else if (margin >= 2)
            {
                if (++_slackStreak >= 3)
                {
                    _slackStreak = 0;
                    if (InputDelay > InputDelayMin)
                    {
                        InputDelay--;
                    }
                }
            }
            else
            {
                _slackStreak = 0;
            }
        }

        void DecodeBundle(byte[] bytes, int offset, int len)
        {
            int end = offset + len;
            if (len < 6 || bytes[offset] != KindBundle)
            {
                return;
            }

            uint tick = U32(bytes, offset + 1);
            int count = bytes[offset + 5];
            offset += 6;
            if (tick + 1 > ServerTick)
            {
                ServerTick = tick + 1;
            }

            if (!_started)
            {
                _started = true;
                _next = tick;
            }
            if (tick < _next || _pending.ContainsKey(tick))
            {
                return;
            }

            var inputs = new Dictionary<ushort, byte[]>(count);
            for (int i = 0; i < count && offset + 3 <= end; i++)
            {
                ushort actor = (ushort)((bytes[offset] << 8) | bytes[offset + 1]);
                int length = bytes[offset + 2];
                offset += 3;
                if (offset + length > end)
                {
                    break;
                }

                var data = new byte[length];
                Array.Copy(bytes, offset, data, 0, length);
                offset += length;
                inputs[actor] = data;
            }
            _pending[tick] = inputs;
        }

        void Drain()
        {
            while (_pending.TryGetValue(_next, out var inputs))
            {
                _pending.Remove(_next);
                var tick = _next;
                _next++;
                Delivered++;
                if (inputs.TryGetValue(CommandActor, out var cmds))
                {
                    inputs.Remove(CommandActor);
                    DispatchCommands(tick, cmds);
                }
                OnTick?.Invoke(tick, inputs);
            }
            // Ticks older than the redundancy window that never arrived are lost for good only if the
            // node skipped them; the node re-sends 3 ticks per datagram and the journal tail is reliable.
            while (_pending.Count > 0)
            {
                var enumerator = _pending.GetEnumerator();
                enumerator.MoveNext();
                var oldest = enumerator.Current.Key;
                if (oldest <= _next + 600)
                {
                    break;
                }

                _pending.Remove(oldest);
                Dropped++;
            }
        }

        /// <summary>Queues this tick's local input (sent with the previous two for redundancy). The room takes one
        /// input per tick: called more often than that it sends nothing, so sample the input again next time.</summary>
        public void SendInput(byte[] input)
        {
            uint tick = Math.Max(ServerTick + InputDelay, _lastInput + 1);
            if (tick > ServerTick + InputDelay + 1)
            {
                return;
            }

            _lastInput = tick;
            _recentInputs.Add(input);
            int keep = Math.Max(1, Math.Min(8, InputRedundancy));
            while (_recentInputs.Count > keep)
            {
                _recentInputs.RemoveAt(0);
            }

            _send(
                Channel.Unreliable,
                SyncWorld.EncodeInput(tick - (uint)(_recentInputs.Count - 1), _recentInputs)
            );
        }

        /// <summary>Reliable command outside the input stream (buy, surrender, pause): the server stamps it onto the next tick and every client and the referee get it via OnCommand, journaled and replayed. 1..1024 bytes.</summary>

        public void SendCommand(byte[] data)
        {
            if (data == null || data.Length == 0 || data.Length > 1024)
            {
                throw new ArgumentException("1..1024 bytes", nameof(data));
            }

            var bytes = new byte[1 + data.Length];
            bytes[0] = KindCommand;
            Buffer.BlockCopy(data, 0, bytes, 1, data.Length);

            _send(Channel.Reliable, bytes);
        }

        void DispatchCommands(uint tick, byte[] cmds)
        {
            int index = 0;

            while (index + 4 <= cmds.Length)
            {
                ushort actor = (ushort)((cmds[index] << 8) | cmds[index + 1]);
                int len = (cmds[index + 2] << 8) | cmds[index + 3];
                index += 4;

                if (index + len > cmds.Length)
                {
                    break;
                }

                var data = new byte[len];
                Buffer.BlockCopy(cmds, index, data, 0, len);
                index += len;

                OnCommand?.Invoke(tick, actor, data);
            }
        }

        public void ReportHash(uint tick, ulong hash)
        {
            var bytes = new byte[13];
            bytes[0] = KindHash;
            W32(bytes, 1, tick);
            W64(bytes, 5, hash);
            _send(Channel.Unreliable, bytes);
        }

        /// <summary>Master only, in answer to OnCheckpointRequested: confirmed state at `tick` (≤ requested).</summary>
        public void SendCheckpoint(uint tick, ulong hash, byte[] state)
        {
            var bytes = new byte[13 + state.Length];
            bytes[0] = KindCheckpoint;
            W32(bytes, 1, tick);
            W64(bytes, 5, hash);
            Array.Copy(state, 0, bytes, 13, state.Length);
            _send(Channel.Reliable, bytes);
        }

        /// <summary>Answer to OnDumpRequested: your state at `tick` (≤ 60 KiB), stored by the node for 24 h next to
        /// the master's, so the two can be diffed offline.</summary>
        public void SendDump(uint tick, ulong hash, byte[] state)
        {
            var bytes = new byte[13 + state.Length];
            bytes[0] = KindDump;
            W32(bytes, 1, tick);
            W64(bytes, 5, hash);
            Array.Copy(state, 0, bytes, 13, state.Length);
            _send(Channel.Reliable, bytes);
        }

        /// <summary>FNV-1a 64 over bytes: a simple, portable state hash.</summary>
        public static ulong Fnv1a(byte[] data)
        {
            ulong hash = 14695981039346656037UL;
            foreach (var value in data)
            {
                hash ^= value;
                hash *= 1099511628211UL;
            }
            return hash;
        }

        static uint U32(byte[] bytes, int offset) =>
            ((uint)bytes[offset] << 24)
            | ((uint)bytes[offset + 1] << 16)
            | ((uint)bytes[offset + 2] << 8)
            | bytes[offset + 3];

        static ulong U64(byte[] bytes, int offset) =>
            ((ulong)U32(bytes, offset) << 32) | U32(bytes, offset + 4);

        static void W32(byte[] bytes, int offset, uint value)
        {
            bytes[offset] = (byte)(value >> 24);
            bytes[offset + 1] = (byte)(value >> 16);
            bytes[offset + 2] = (byte)(value >> 8);
            bytes[offset + 3] = (byte)value;
        }

        static void W64(byte[] bytes, int offset, ulong value)
        {
            W32(bytes, offset, (uint)(value >> 32));
            W32(bytes, offset + 4, (uint)value);
        }
    }
}
