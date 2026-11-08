// Tick-based timer (Fusion's TickTimer): created against the current server tick, compared against it
// later, so every peer agrees on when it expires. Pure C#, serializable as two ticks.
using System;

namespace Aurix.Net.Managed
{
    [Serializable]
    public struct TickTimer : IEquatable<TickTimer>
    {
        public uint Start, End;

        public static readonly TickTimer None = default;
        public static TickTimer CreateFromTicks(uint now, uint ticks) => new TickTimer { Start = now, End = now + Math.Max(1u, ticks) };
        public static TickTimer CreateFromSeconds(uint now, float seconds, int tickHz) => CreateFromTicks(now, (uint)Math.Ceiling(Math.Max(0f, seconds) * Math.Max(1, tickHz)));

        /// <summary>False for <see cref="None"/>; true until reset (also after it expired).</summary>
        public bool IsRunning => End != 0;
        public bool Expired(uint now) => IsRunning && now >= End;
        /// <summary>Running and not yet expired.</summary>
        public bool IsActive(uint now) => IsRunning && now < End;
        public uint RemainingTicks(uint now) => IsActive(now) ? End - now : 0;
        public float RemainingSeconds(uint now, int tickHz) => RemainingTicks(now) / (float)Math.Max(1, tickHz);
        /// <summary>0..1 progress since creation.</summary>
        public float Normalized(uint now) => !IsRunning || End <= Start ? 1f : Math.Min(1f, Math.Max(0f, (now - (float)Start) / (End - Start)));

        public bool Equals(TickTimer o) => Start == o.Start && End == o.End;
        public override bool Equals(object obj) => obj is TickTimer t && Equals(t);
        public override int GetHashCode() => (int)(Start * 31 + End);
        public override string ToString() => IsRunning ? $"TickTimer({Start}->{End})" : "TickTimer.None";
    }
}
