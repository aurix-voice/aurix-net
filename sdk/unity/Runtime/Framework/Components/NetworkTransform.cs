// Replicates position / rotation / scale of a networked object. The controller (owner when
// ownerAuth, else the server) samples the Transform every network tick and sends what changed
// (quantized); remote copies buffer the samples and interpolate `interpolationTicks` behind the
// newest one, snapping when the object teleported farther than `teleportDistance`.
using System.Collections.Generic;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    public enum SyncMode : byte { No, Local, World }

    /// <summary>Module carrying one transform sample per tick; interpolation lives in <see cref="NetworkTransform"/>.</summary>
    public sealed class TransformSync : NetworkModule
    {
        public struct Sample { public uint tick; public Vector3 pos; public Quaternion rot; public Vector3 scale; public uint parent; }
        public Sample current;
        public bool syncPosition = true, syncRotation = true, syncScale, syncParent;
        public float positionResolution = 0.001f, positionRange = 8192f;
        public readonly List<Sample> buffer = new List<Sample>(16);
        public event System.Action<Sample> onSample;
        Sample _lastSent; bool _sentOnce;

        public TransformSync(bool ownerAuth = true, float sendInterval = 0f) { this.ownerAuth = ownerAuth; sendIntervalInSeconds = sendInterval; }

        /// <summary>Controller side: record the current transform; marks dirty when something moved.</summary>
        public void Set(Vector3 pos, Quaternion rot, Vector3 scale, uint parent = 0)
        {
            current.pos = pos; current.rot = rot; current.scale = scale; current.parent = parent;
            if (!_sentOnce || (syncPosition && pos != _lastSent.pos) || (syncRotation && Quaternion.Angle(rot, _lastSent.rot) > 0.01f) || (syncScale && scale != _lastSent.scale) || (syncParent && parent != _lastSent.parent)) isDirty = true;
        }

        public override void WriteFull(BitWriter w) => WriteSample(w, current, true);
        public override void ReadFull(BitReader r) { ReadSample(r, ref current, true); buffer.Clear(); Push(current); }
        public override void WriteDelta(BitWriter w) { WriteSample(w, current, false); _lastSent = current; _sentOnce = true; }
        public override void ReadDelta(BitReader r) { ReadSample(r, ref current, false); current.tick = lastReceivedTick; Push(current); }

        void Push(Sample s)
        {
            if (buffer.Count > 0 && buffer[buffer.Count - 1].tick >= s.tick) buffer.RemoveAt(buffer.Count - 1);
            buffer.Add(s); while (buffer.Count > 16) buffer.RemoveAt(0);
            onSample?.Invoke(s);
        }

        void WriteSample(BitWriter w, in Sample s, bool full)
        {
            bool p = syncPosition && (full || s.pos != _lastSent.pos), q = syncRotation && (full || Quaternion.Angle(s.rot, _lastSent.rot) > 0.01f), sc = syncScale && (full || s.scale != _lastSent.scale);
            bool pa = syncParent && (full || s.parent != _lastSent.parent);
            w.WriteBool(p); w.WriteBool(q); w.WriteBool(sc); w.WriteBool(pa);
            if (pa) w.WriteBits(s.parent, 32);
            if (p) { w.WriteQuantized(s.pos.x, -positionRange, positionRange, positionResolution); w.WriteQuantized(s.pos.y, -positionRange, positionRange, positionResolution); w.WriteQuantized(s.pos.z, -positionRange, positionRange, positionResolution); }
            if (q) WriteQuat(w, s.rot);
            if (sc) { w.WriteF32(s.scale.x); w.WriteF32(s.scale.y); w.WriteF32(s.scale.z); }
        }

        void ReadSample(BitReader r, ref Sample s, bool full)
        {
            bool p = r.ReadBool(), q = r.ReadBool(), sc = r.ReadBool(), pa = r.ReadBool();
            if (pa) s.parent = (uint)r.ReadBits(32);
            if (p) s.pos = new Vector3(r.ReadQuantized(-positionRange, positionRange, positionResolution), r.ReadQuantized(-positionRange, positionRange, positionResolution), r.ReadQuantized(-positionRange, positionRange, positionResolution));
            if (q) s.rot = ReadQuat(r);
            if (sc) s.scale = new Vector3(r.ReadF32(), r.ReadF32(), r.ReadF32());
        }

        /// <summary>Smallest-three quaternion packing: 2 + 3×10 bits.</summary>
        public static void WriteQuat(BitWriter w, Quaternion q)
        {
            float[] c = { q.x, q.y, q.z, q.w }; int big = 0; for (int i = 1; i < 4; i++) if (Mathf.Abs(c[i]) > Mathf.Abs(c[big])) big = i;
            float sign = c[big] < 0 ? -1f : 1f; w.WriteBits((ulong)big, 2);
            for (int i = 0; i < 4; i++) if (i != big) w.WriteQuantized(c[i] * sign, -0.7071068f, 0.7071068f, 0.0014f);
        }

        public static Quaternion ReadQuat(BitReader r)
        {
            int big = (int)r.ReadBits(2); float[] c = new float[4]; float sum = 0;
            for (int i = 0; i < 4; i++) if (i != big) { c[i] = r.ReadQuantized(-0.7071068f, 0.7071068f, 0.0014f); sum += c[i] * c[i]; }
            c[big] = Mathf.Sqrt(Mathf.Max(0, 1 - sum));
            return new Quaternion(c[0], c[1], c[2], c[3]);
        }
    }

    [DisallowMultipleComponent]
    public class NetworkTransform : NetworkBehaviour
    {
        [Tooltip("Who drives the transform: the owner (true) or the server (false).")] public bool ownerAuth = true;
        public SyncMode syncPosition = SyncMode.World, syncRotation = SyncMode.World;
        public bool syncScale;
        [Tooltip("Replicate the parent NetworkIdentity: remotes re-parent and the pose is sent in the parent's local space while attached.")] public bool syncParent;
        /// <summary>Controller: the networked parent this object is attached to (null = none / a non-networked parent).</summary>
        public NetworkIdentity parentIdentity { get; private set; }
        [Tooltip("Sends per second from the controller (0 = every network tick).")] public float sendRate;
        [Tooltip("Remote copies render this many ticks behind the newest sample.")] public float interpolationTicks = 2f;
        [Tooltip("Snap instead of interpolating when the remote jumped farther than this.")] public float teleportDistance = 5f;
        public float positionResolution = 0.001f, positionRange = 8192f;

        public TransformSync sync;
        float _render; uint _newest;

        /// <summary>Latest replicated pose (remote copies) or the last sampled one (controller).</summary>
        public Vector3 latestPosition => sync.current.pos;
        public Quaternion latestRotation => sync.current.rot;

        protected virtual void Awake() => EnsureSync();
        protected override void OnBeforeBind() => EnsureSync();

        void EnsureSync()
        {
            if (sync != null) return;
            sync = new TransformSync(ownerAuth, sendRate > 0 ? 1f / sendRate : 0f) { syncPosition = syncPosition != SyncMode.No, syncRotation = syncRotation != SyncMode.No, syncScale = syncScale, syncParent = syncParent, positionResolution = positionResolution, positionRange = positionRange };
            Sample();
            sync.onSample += s => { if (!isController && sync.buffer.Count > 1 && Vector3.Distance(sync.buffer[sync.buffer.Count - 2].pos, s.pos) > teleportDistance) { sync.buffer.RemoveRange(0, sync.buffer.Count - 1); _render = s.tick; } _newest = s.tick; };
        }

        protected override void OnSpawned(bool asServer)
        {
            if (isController) Sample(); else { Apply(sync.current.pos, sync.current.rot, sync.current.scale); _render = _newest = sync.current.tick; }
        }

        public override void OnTick(uint tick)
        {
            if (!isController) return;
            Sample(); sync.current.tick = tick;
        }

        protected virtual void Update() => RenderUpdate(Time.unscaledDeltaTime);

        /// <summary>Per-frame work (called from Update): controller samples the transform, remotes interpolate.</summary>
        public void RenderUpdate(float dt)
        {
            if (!isSpawned) return;
            if (isController) { Sample(); return; }
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
            var t = transform;
            bool local = attached;
            sync.Set(syncPosition == SyncMode.Local || local ? t.localPosition : t.position, syncRotation == SyncMode.Local || local ? t.localRotation : t.rotation, t.localScale, attached ? parentIdentity.objectId : 0);
        }

        uint _appliedParent;

        protected virtual void Apply(Vector3 pos, Quaternion rot, Vector3 scale)
        {
            var t = transform;
            if (syncParent && sync.current.parent != _appliedParent)
            {
                _appliedParent = sync.current.parent;
                NetworkIdentity p = null;
                if (_appliedParent != 0 && networkManager != null && !networkManager.TryGetIdentity(_appliedParent, out p)) p = null;
                parentIdentity = p;
                t.SetParent(p != null ? p.transform : null, false);
            }
            bool local = syncParent && _appliedParent != 0;
            if (syncPosition == SyncMode.World && !local) t.position = pos; else if (syncPosition != SyncMode.No) t.localPosition = pos;
            if (syncRotation == SyncMode.World && !local) t.rotation = rot; else if (syncRotation != SyncMode.No) t.localRotation = rot;
            if (syncScale) t.localScale = scale;
        }

        void Interpolate(float dt)
        {
            var buf = sync.buffer; if (buf.Count == 0) return;
            float target = _newest - interpolationTicks;
            float hz = networkManager != null ? networkManager.tickRate : 30;
            _render += dt * hz;
            if (_render > target + 1) _render = target + 1; else if (_render < target - 4) _render = target - 4; // keep near the target, never run far ahead
            TransformSync.Sample a = buf[0], b = buf[buf.Count - 1];
            for (int i = 0; i + 1 < buf.Count; i++) if (buf[i].tick <= _render && buf[i + 1].tick >= _render) { a = buf[i]; b = buf[i + 1]; break; }
            float t = b.tick > a.tick ? Mathf.Clamp01((_render - a.tick) / (b.tick - a.tick)) : 1f;
            if (_render >= b.tick) t = 1f;
            Apply(Vector3.Lerp(a.pos, b.pos, t), Quaternion.Slerp(a.rot, b.rot, t), Vector3.Lerp(a.scale, b.scale, t));
        }

        /// <summary>Controller: move instantly on every peer (no interpolation across the jump).</summary>
        public void Teleport(Vector3 pos, Quaternion rot)
        {
            transform.position = pos; transform.rotation = rot; Sample();
        }
    }
}
