using System;

namespace Aurix.Net.Unity.Framework
{
    /// <summary>Tick-based timer that every peer evaluates identically: store it in a SyncVar or a
    /// predicted state and ask <see cref="Expired"/> against the manager's tick instead of using wall-clock time.</summary>
    [Serializable]
    public struct TickTimer : IEquatable<TickTimer>
    {
        /// <summary>Tick at which the timer expires; 0 = not running.</summary>
        public uint Target;

        public static TickTimer None => default;

        public static TickTimer CreateFromTicks(NetworkManager nm, uint ticks) =>
            new TickTimer { Target = Math.Max(1, nm.tick + ticks) };

        public static TickTimer CreateFromSeconds(NetworkManager nm, float seconds) =>
            CreateFromTicks(
                nm,
                (uint)Math.Ceiling(Math.Max(0f, seconds) * Math.Max(1, nm.tickRate))
            );

        public bool IsRunning => Target != 0;

        public bool Expired(NetworkManager nm) => Target != 0 && nm.tick >= Target;

        /// <summary>True while running and not yet expired.</summary>
        public bool IsRunningAndNotExpired(NetworkManager nm) => Target != 0 && nm.tick < Target;

        public uint RemainingTicks(NetworkManager nm) =>
            Target == 0 || nm.tick >= Target ? 0 : Target - nm.tick;

        public float RemainingTime(NetworkManager nm) =>
            RemainingTicks(nm) / (float)Math.Max(1, nm.tickRate);

        public bool Equals(TickTimer other) => Target == other.Target;

        public override bool Equals(object target) => target is TickTimer timer && Equals(timer);

        public override int GetHashCode() => (int)Target;

        public override string ToString() =>
            Target == 0 ? "TickTimer.None" : $"TickTimer(until {Target})";
    }
}
