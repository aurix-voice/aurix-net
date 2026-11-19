// Replicates position / rotation / scale of a networked object. The controller (owner when
// ownerAuth, else the server) samples the Transform every network tick and sends what changed
// (quantized); remote copies buffer the samples and interpolate `interpolationTicks` behind the
// newest one, snapping when the object teleported farther than `teleportDistance`.
using System;
using System.Collections.Generic;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    public enum SyncMode : byte
    {
        No,
        Local,
        World,
    }

    /// <summary>Module carrying one transform sample per tick; interpolation lives in <see cref="NetworkTransform"/>.</summary>
    public enum AdaptiveSyncLevel : byte
    {
        Off,
        Conservative,
        Balanced,
        Aggressive,
    }

    /// <summary>
    /// Adaptive sync: the controller skips a send while receivers can reconstruct the motion from the last two samples
    /// (default: linear extrapolation) within the level's tolerance; <see cref="maxSendInterval"/> bounds how long a
    /// skip streak may last. Receivers use the same reconstruction when their buffer runs dry. Subclass and override
    /// <see cref="TryReconstruct"/> for other motion models; see <see cref="CompositeTransformSyncStrategy"/>.
    /// </summary>
    public class TransformSyncStrategy
    {
        public float maxSendInterval = 0.3f;
        public float positionTolerance = 0.02f,
            angleTolerance = 1f,
            scaleTolerance = 0.01f;

        public static TransformSyncStrategy For(AdaptiveSyncLevel level)
        {
            switch (level)
            {
                case AdaptiveSyncLevel.Conservative:
                    return new TransformSyncStrategy
                    {
                        positionTolerance = 0.01f,
                        angleTolerance = 0.5f,
                        maxSendInterval = 0.2f,
                    };
                case AdaptiveSyncLevel.Balanced:
                    return new TransformSyncStrategy();
                case AdaptiveSyncLevel.Aggressive:
                    return new TransformSyncStrategy
                    {
                        positionTolerance = 0.08f,
                        angleTolerance = 3f,
                        maxSendInterval = 0.5f,
                    };
                default:
                    return null;
            }
        }

        /// <summary>Predict the sample at <paramref name="tick"/> given the two last known samples; false = cannot reconstruct.</summary>
        public virtual bool TryReconstruct(
            in TransformSync.Sample prev,
            in TransformSync.Sample from,
            float tick,
            out TransformSync.Sample result
        )
        {
            result = from;
            if (from.tick <= prev.tick)
            {
                return false;
            }

            float fraction = (tick - from.tick) / (from.tick - prev.tick);
            result.pos = from.pos + (from.pos - prev.pos) * fraction;
            result.rot = Quaternion.Slerp(prev.rot, from.rot, 1f + fraction);
            result.scale = from.scale + (from.scale - prev.scale) * fraction;
            result.tick = (uint)Math.Max(0, Math.Round(tick));
            return true;
        }

        public bool Within(
            in TransformSync.Sample predicted,
            in TransformSync.Sample actual,
            bool pos,
            bool rot,
            bool scale
        ) =>
            (!pos || Vector3.Distance(predicted.pos, actual.pos) <= positionTolerance)
            && (!rot || Quaternion.Angle(predicted.rot, actual.rot) <= angleTolerance)
            && (!scale || Vector3.Distance(predicted.scale, actual.scale) <= scaleTolerance);
    }

    /// <summary>Tries each strategy in order; the first that reconstructs wins.</summary>
    public class CompositeTransformSyncStrategy : TransformSyncStrategy
    {
        public TransformSyncStrategy[] strategies;

        public CompositeTransformSyncStrategy(params TransformSyncStrategy[] strategies)
        {
            this.strategies = strategies;
        }

        public override bool TryReconstruct(
            in TransformSync.Sample prev,
            in TransformSync.Sample from,
            float tick,
            out TransformSync.Sample result
        )
        {
            if (strategies != null)
            {
                foreach (var st in strategies)
                {
                    if (st != null && st.TryReconstruct(prev, from, tick, out result))
                    {
                        return true;
                    }
                }
            }

            return base.TryReconstruct(prev, from, tick, out result);
        }
    }

    public sealed class TransformSync : NetworkModule
    {
        /// <summary>Adaptive sync strategy (null = send every change).</summary>
        public TransformSyncStrategy strategy;
        public float tickRate = 30f;
        public int sendsMade { get; private set; }
        public int sendsSkipped { get; private set; }
        Sample _prevSent;
        int _sentCount;

        public struct Sample
        {
            public uint tick;
            public Vector3 pos;
            public Quaternion rot;
            public Vector3 scale;
            public uint parent;
        }

        public Sample current;
        public bool syncPosition = true,
            syncRotation = true,
            syncScale,
            syncParent;
        public float positionResolution = 0.001f,
            positionRange = 8192f;
        public readonly List<Sample> buffer = new List<Sample>(16);
        public event System.Action<Sample> onSample;
        Sample _lastSent;
        bool _sentOnce;

        public TransformSync(bool ownerAuth = true, float sendInterval = 0f)
        {
            this.ownerAuth = ownerAuth;
            sendIntervalInSeconds = sendInterval;
        }

        /// <summary>Controller side: record the current transform; marks dirty when something moved.</summary>
        public void Set(Vector3 pos, Quaternion rot, Vector3 scale, uint parent = 0)
        {
            current.pos = pos;
            current.rot = rot;
            current.scale = scale;
            current.parent = parent;
            bool changed =
                !_sentOnce
                || (syncPosition && pos != _lastSent.pos)
                || (syncRotation && Quaternion.Angle(rot, _lastSent.rot) > 0.01f)
                || (syncScale && scale != _lastSent.scale)
                || (syncParent && parent != _lastSent.parent);
            Sample predicted = default;
            bool reconstructs =
                strategy != null
                && _sentCount >= 2
                && parent == _lastSent.parent
                && current.tick > _lastSent.tick
                && strategy.TryReconstruct(_prevSent, _lastSent, current.tick, out predicted);
            bool within =
                reconstructs
                && strategy.Within(predicted, current, syncPosition, syncRotation, syncScale);
            if (!changed)
            {
                // the receiver extrapolates from the last two samples: after a stop it needs a second one at rest
                if (reconstructs && !within)
                {
                    isDirty = true;
                }

                return;
            }

            if (
                within
                && (current.tick - _lastSent.tick) / Math.Max(1f, tickRate)
                    < strategy.maxSendInterval
            )
            {
                sendsSkipped++;
                isDirty = false;
                return;
            }
            isDirty = true;
        }

        public override void WriteFull(BitWriter writer) => WriteSample(writer, current, true);

        public override void ReadFull(BitReader reader)
        {
            ReadSample(reader, ref current, true);
            buffer.Clear();
            Push(current);
        }

        public override void WriteDelta(BitWriter writer)
        {
            WriteSample(writer, current, false);
            _prevSent = _lastSent;
            _lastSent = current;
            _sentOnce = true;
            _sentCount++;
            sendsMade++;
        }

        public override void ReadDelta(BitReader reader)
        {
            ReadSample(reader, ref current, false);
            current.tick = lastReceivedTick;
            Push(current);
        }

        void Push(Sample sample)
        {
            if (buffer.Count > 0 && buffer[buffer.Count - 1].tick >= sample.tick)
            {
                buffer.RemoveAt(buffer.Count - 1);
            }

            buffer.Add(sample);
            while (buffer.Count > 16)
            {
                buffer.RemoveAt(0);
            }

            onSample?.Invoke(sample);
        }

        void WriteSample(BitWriter writer, in Sample sample, bool full)
        {
            bool hasPosition = syncPosition && (full || sample.pos != _lastSent.pos),
                hasRotation =
                    syncRotation && (full || Quaternion.Angle(sample.rot, _lastSent.rot) > 0.01f),
                sc = syncScale && (full || sample.scale != _lastSent.scale);
            bool pa = syncParent && (full || sample.parent != _lastSent.parent);
            writer.WriteBool(hasPosition);
            writer.WriteBool(hasRotation);
            writer.WriteBool(sc);
            writer.WriteBool(pa);
            if (pa)
            {
                writer.WriteBits(sample.parent, 32);
            }

            if (hasPosition)
            {
                writer.WriteQuantized(
                    sample.pos.x,
                    -positionRange,
                    positionRange,
                    positionResolution
                );
                writer.WriteQuantized(
                    sample.pos.y,
                    -positionRange,
                    positionRange,
                    positionResolution
                );
                writer.WriteQuantized(
                    sample.pos.z,
                    -positionRange,
                    positionRange,
                    positionResolution
                );
            }
            if (hasRotation)
            {
                WriteQuat(writer, sample.rot);
            }

            if (sc)
            {
                writer.WriteF32(sample.scale.x);
                writer.WriteF32(sample.scale.y);
                writer.WriteF32(sample.scale.z);
            }
        }

        void ReadSample(BitReader reader, ref Sample sample, bool full)
        {
            bool hasPosition = reader.ReadBool(),
                hasRotation = reader.ReadBool(),
                sc = reader.ReadBool(),
                pa = reader.ReadBool();
            if (pa)
            {
                sample.parent = (uint)reader.ReadBits(32);
            }

            if (hasPosition)
            {
                sample.pos = new Vector3(
                    reader.ReadQuantized(-positionRange, positionRange, positionResolution),
                    reader.ReadQuantized(-positionRange, positionRange, positionResolution),
                    reader.ReadQuantized(-positionRange, positionRange, positionResolution)
                );
            }

            if (hasRotation)
            {
                sample.rot = ReadQuat(reader);
            }

            if (sc)
            {
                sample.scale = new Vector3(reader.ReadF32(), reader.ReadF32(), reader.ReadF32());
            }
        }

        /// <summary>Smallest-three quaternion packing: 2 + 3×10 bits.</summary>
        public static void WriteQuat(BitWriter writer, Quaternion rotation)
        {
            float[] components = { rotation.x, rotation.y, rotation.z, rotation.w };
            int big = 0;
            for (int i = 1; i < 4; i++)
            {
                if (Mathf.Abs(components[i]) > Mathf.Abs(components[big]))
                {
                    big = i;
                }
            }

            float sign = components[big] < 0 ? -1f : 1f;
            writer.WriteBits((ulong)big, 2);
            for (int i = 0; i < 4; i++)
            {
                if (i != big)
                {
                    writer.WriteQuantized(components[i] * sign, -0.7071068f, 0.7071068f, 0.0014f);
                }
            }
        }

        public static Quaternion ReadQuat(BitReader reader)
        {
            int big = (int)reader.ReadBits(2);
            float[] components = new float[4];
            float sum = 0;
            for (int i = 0; i < 4; i++)
            {
                if (i != big)
                {
                    components[i] = reader.ReadQuantized(-0.7071068f, 0.7071068f, 0.0014f);
                    sum += components[i] * components[i];
                }
            }

            components[big] = Mathf.Sqrt(Mathf.Max(0, 1 - sum));
            return new Quaternion(components[0], components[1], components[2], components[3]);
        }
    }

    [DisallowMultipleComponent]
    public class NetworkTransform : NetworkBehaviour
    {
        [Tooltip("Who drives the transform: the owner (true) or the server (false).")]
        public bool ownerAuth = true;
        public SyncMode syncPosition = SyncMode.World,
            syncRotation = SyncMode.World;
        public bool syncScale;

        [Tooltip(
            "Adaptive sync: skip sends while receivers can reconstruct the motion (Off = send every change)."
        )]
        public AdaptiveSyncLevel adaptiveSync = AdaptiveSyncLevel.Off;

        [Tooltip(
            "Replicate the parent NetworkIdentity: remotes re-parent and the pose is sent in the parent's local space while attached."
        )]
        public bool syncParent;

        /// <summary>Controller: the networked parent this object is attached to (null = none / a non-networked parent).</summary>
        public NetworkIdentity parentIdentity { get; private set; }

        [Tooltip("Sends per second from the controller (0 = every network tick).")]
        public float sendRate;

        [Tooltip("Remote copies render this many ticks behind the newest sample.")]
        public float interpolationTicks = 2f;

        [Tooltip("Snap instead of interpolating when the remote jumped farther than this.")]
        public float teleportDistance = 5f;
        public float positionResolution = 0.001f,
            positionRange = 8192f;

        public TransformSync sync;
        float _render;
        uint _newest;

        /// <summary>Latest replicated pose (remote copies) or the last sampled one (controller).</summary>
        public Vector3 latestPosition => sync.current.pos;
        public Quaternion latestRotation => sync.current.rot;

        protected virtual void Awake() => EnsureSync();

        protected override void OnBeforeBind() => EnsureSync();

        void EnsureSync()
        {
            if (sync != null)
            {
                return;
            }

            sync = new TransformSync(ownerAuth, sendRate > 0 ? 1f / sendRate : 0f)
            {
                syncPosition = syncPosition != SyncMode.No,
                syncRotation = syncRotation != SyncMode.No,
                syncScale = syncScale,
                syncParent = syncParent,
                strategy = TransformSyncStrategy.For(adaptiveSync),
                positionResolution = positionResolution,
                positionRange = positionRange,
            };
            Sample();
            sync.onSample += sample =>
            {
                if (
                    !isController
                    && sync.buffer.Count > 1
                    && Vector3.Distance(sync.buffer[sync.buffer.Count - 2].pos, sample.pos)
                        > teleportDistance
                )
                {
                    sync.buffer.RemoveRange(0, sync.buffer.Count - 1);
                    _render = sample.tick;
                }
                _newest = sample.tick;
            };
        }

        protected override void OnSpawned(bool asServer)
        {
            if (isController)
            {
                Sample();
            }
            else
            {
                Apply(sync.current.pos, sync.current.rot, sync.current.scale);
                _render = _newest = sync.current.tick;
            }
        }

        public override void OnTick(uint tick)
        {
            if (!isController)
            {
                return;
            }

            sync.current.tick = tick;
            if (networkManager != null)
            {
                sync.tickRate = networkManager.tickRate;
            }

            Sample();
        }

        /// <summary>Inject a custom adaptive sync strategy (null = the one for <see cref="adaptiveSync"/>).</summary>
        public void SetSyncStrategy(TransformSyncStrategy strategy)
        {
            EnsureSync();
            sync.strategy = strategy ?? TransformSyncStrategy.For(adaptiveSync);
        }

        protected virtual void Update() => RenderUpdate(Time.unscaledDeltaTime);

        /// <summary>Per-frame work (called from Update): controller samples the transform, remotes interpolate.</summary>
        public void RenderUpdate(float dt)
        {
            if (!isSpawned)
            {
                return;
            }

            if (isController)
            {
                Sample();
                return;
            }
            Interpolate(dt);
        }

        /// <summary>Controller: attach to another networked object (or detach with null); replicated when syncParent is on.</summary>
        public void SetParent(NetworkIdentity parent, bool worldPositionStays = true)
        {
            parentIdentity = parent;
            transform.SetParent(parent != null ? parent.transform : null, worldPositionStays);
            Sample();
        }

        bool attached => syncParent && parentIdentity != null;

        void Sample()
        {
            var self = transform;
            bool local = attached;
            sync.Set(
                syncPosition == SyncMode.Local || local ? self.localPosition : self.position,
                syncRotation == SyncMode.Local || local ? self.localRotation : self.rotation,
                self.localScale,
                attached ? parentIdentity.objectId : 0
            );
        }

        uint _appliedParent;

        protected virtual void Apply(Vector3 pos, Quaternion rot, Vector3 scale)
        {
            var self = transform;
            if (syncParent && sync.current.parent != _appliedParent)
            {
                _appliedParent = sync.current.parent;
                NetworkIdentity parent = null;
                if (
                    _appliedParent != 0
                    && networkManager != null
                    && !networkManager.TryGetIdentity(_appliedParent, out parent)
                )
                {
                    parent = null;
                }

                parentIdentity = parent;
                self.SetParent(parent != null ? parent.transform : null, false);
            }
            bool local = syncParent && _appliedParent != 0;
            if (syncPosition == SyncMode.World && !local)
            {
                self.position = pos;
            }
            else if (syncPosition != SyncMode.No)
            {
                self.localPosition = pos;
            }

            if (syncRotation == SyncMode.World && !local)
            {
                self.rotation = rot;
            }
            else if (syncRotation != SyncMode.No)
            {
                self.localRotation = rot;
            }

            if (syncScale)
            {
                self.localScale = scale;
            }
        }

        void Interpolate(float dt)
        {
            var buf = sync.buffer;
            if (buf.Count == 0)
            {
                return;
            }

            float target = _newest - interpolationTicks;
            float hz = networkManager != null ? networkManager.tickRate : 30;
            _render += dt * hz;
            float maxAhead =
                sync.strategy != null ? Math.Max(1f, sync.strategy.maxSendInterval * hz) : 1f; // adaptive sync: may run past the newest sample and extrapolate
            if (_render > target + maxAhead)
            {
                _render = target + maxAhead;
            }
            else if (_render < target - 4)
            {
                _render = target - 4; // keep near the target, never run far ahead
            }

            TransformSync.Sample older = buf[0],
                newer = buf[buf.Count - 1];
            for (int i = 0; i + 1 < buf.Count; i++)
            {
                if (buf[i].tick <= _render && buf[i + 1].tick >= _render)
                {
                    older = buf[i];
                    newer = buf[i + 1];
                    break;
                }
            }

            float fraction =
                newer.tick > older.tick
                    ? Mathf.Clamp01((_render - older.tick) / (newer.tick - older.tick))
                    : 1f;
            if (_render >= newer.tick)
            {
                fraction = 1f;
            }

            if (
                _render > newer.tick
                && sync.strategy != null
                && buf.Count >= 2
                && (_render - newer.tick) / hz <= sync.strategy.maxSendInterval
                && sync.strategy.TryReconstruct(buf[buf.Count - 2], newer, _render, out var ex)
            )
            {
                Apply(ex.pos, ex.rot, ex.scale);
                return;
            }
            Apply(
                Vector3.Lerp(older.pos, newer.pos, fraction),
                Quaternion.Slerp(older.rot, newer.rot, fraction),
                Vector3.Lerp(older.scale, newer.scale, fraction)
            );
        }

        /// <summary>Controller: move instantly on every peer (no interpolation across the jump).</summary>
        public void Teleport(Vector3 pos, Quaternion rot)
        {
            transform.position = pos;
            transform.rotation = rot;
            Sample();
        }
    }
}
