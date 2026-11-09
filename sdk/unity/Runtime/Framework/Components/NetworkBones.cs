// Aurix Net Unity SDK — replicate skeleton bones (local pose) for ragdolls / procedural animation.
// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using UnityEngine;
using Aurix.Net.Managed;

namespace Aurix.Net.Unity.Framework
{
    /// <summary>
    /// Replicates the local position/rotation/scale of every bone gathered from the SkinnedMeshRenderers under this
    /// object plus <see cref="extraBones"/>. Only bones that moved beyond the accuracy thresholds are sent; remotes
    /// smooth toward the latest pose. Set bones before spawn.
    /// </summary>
    public class NetworkBones : NetworkBehaviour
    {
        [Tooltip("Owner samples and sends (otherwise the server).")] public bool ownerAuth = true;
        [Tooltip("Max sends per second.")] public float sendRate = 10f;
        [Tooltip("Extra bones to sync on top of the ones gathered from SkinnedMeshRenderers.")] public Transform[] extraBones = Array.Empty<Transform>();
        public bool syncPosition = true, syncRotation = true, syncScale;
        public float positionAccuracy = 0.01f, angleAccuracy = 0.5f, scaleAccuracy = 0.05f, positionRange = 64f;
        [Tooltip("Remote smoothing speed (1/s); 0 snaps.")] public float smoothing = 20f;

        BonesSync _sync;
        public int boneCount => _sync?.bones.Count ?? 0;
        public IReadOnlyList<Transform> bones => _sync?.bones;

        protected override void OnBeforeBind()
        {
            _sync = new BonesSync(ownerAuth, sendRate > 0 ? 1f / sendRate : 0f) { owner = this };
            var list = new List<Transform>();
            foreach (var smr in GetComponentsInChildren<SkinnedMeshRenderer>()) if (smr.bones != null) foreach (var b in smr.bones) if (b != null && !list.Contains(b)) list.Add(b);
            foreach (var b in extraBones) if (b != null && !list.Contains(b)) list.Add(b);
            _sync.Configure(list);
        }

        public override void OnTick(uint tick) { if (isController) _sync.Sample(); }
        protected virtual void LateUpdate() => RenderUpdate(Time.deltaTime);
        /// <summary>Remote side: ease bones toward the last received pose.</summary>
        public void RenderUpdate(float dt) { if (isSpawned && !isController) _sync.Apply(smoothing > 0 ? Math.Min(1f, dt * smoothing) : 1f); }
    }

    public sealed class BonesSync : NetworkModule
    {
        internal NetworkBones owner;
        public readonly List<Transform> bones = new List<Transform>();
        Vector3[] _pos = Array.Empty<Vector3>(), _scale = Array.Empty<Vector3>(), _tPos = Array.Empty<Vector3>(), _tScale = Array.Empty<Vector3>();
        Quaternion[] _rot = Array.Empty<Quaternion>(), _tRot = Array.Empty<Quaternion>();
        bool[] _dirty = Array.Empty<bool>();

        public BonesSync(bool ownerAuth, float sendInterval) { this.ownerAuth = ownerAuth; sendIntervalInSeconds = sendInterval; }

        public void Configure(List<Transform> list)
        {
            bones.Clear(); bones.AddRange(list); int n = bones.Count;
            _pos = new Vector3[n]; _scale = new Vector3[n]; _rot = new Quaternion[n]; _tPos = new Vector3[n]; _tScale = new Vector3[n]; _tRot = new Quaternion[n]; _dirty = new bool[n];
            for (int i = 0; i < n; i++) { _pos[i] = _tPos[i] = bones[i].localPosition; _rot[i] = _tRot[i] = bones[i].localRotation; _scale[i] = _tScale[i] = bones[i].localScale; }
        }

        public void Sample()
        {
            for (int i = 0; i < bones.Count; i++)
            {
                var b = bones[i]; bool moved = false;
                if (owner.syncPosition && Vector3.Distance(b.localPosition, _pos[i]) > owner.positionAccuracy) { _pos[i] = b.localPosition; moved = true; }
                if (owner.syncRotation && Quaternion.Angle(b.localRotation, _rot[i]) > owner.angleAccuracy) { _rot[i] = b.localRotation; moved = true; }
                if (owner.syncScale && Vector3.Distance(b.localScale, _scale[i]) > owner.scaleAccuracy) { _scale[i] = b.localScale; moved = true; }
                if (moved) { _dirty[i] = true; isDirty = true; }
            }
        }

        public void Apply(float t)
        {
            for (int i = 0; i < bones.Count; i++)
            {
                var b = bones[i];
                if (owner.syncPosition) b.localPosition = Vector3.Lerp(b.localPosition, _tPos[i], t);
                if (owner.syncRotation) b.localRotation = Quaternion.Slerp(b.localRotation, _tRot[i], t);
                if (owner.syncScale) b.localScale = Vector3.Lerp(b.localScale, _tScale[i], t);
            }
        }

        void WriteBone(BitWriter w, int i)
        {
            if (owner.syncPosition) { float r = owner.positionRange, q = owner.positionAccuracy; w.WriteQuantized(_pos[i].x, -r, r, q); w.WriteQuantized(_pos[i].y, -r, r, q); w.WriteQuantized(_pos[i].z, -r, r, q); }
            if (owner.syncRotation) TransformSync.WriteQuat(w, _rot[i]);
            if (owner.syncScale) { w.WriteF32(_scale[i].x); w.WriteF32(_scale[i].y); w.WriteF32(_scale[i].z); }
        }
        void ReadBone(BitReader r, int i)
        {
            if (owner.syncPosition) { float rg = owner.positionRange, q = owner.positionAccuracy; _tPos[i] = new Vector3(r.ReadQuantized(-rg, rg, q), r.ReadQuantized(-rg, rg, q), r.ReadQuantized(-rg, rg, q)); }
            if (owner.syncRotation) _tRot[i] = TransformSync.ReadQuat(r);
            if (owner.syncScale) _tScale[i] = new Vector3(r.ReadF32(), r.ReadF32(), r.ReadF32());
        }

        public override void WriteFull(BitWriter w) { w.WriteUInt((ulong)bones.Count, 16); for (int i = 0; i < bones.Count; i++) WriteBone(w, i); }
        public override void ReadFull(BitReader r)
        {
            int n = (int)r.ReadUInt(16);
            if (n != bones.Count) { Debug.LogError($"NetworkBones: {n} bones received, {bones.Count} local; skipping"); return; }
            for (int i = 0; i < n; i++) ReadBone(r, i);
            Apply(1f);
        }
        public override void WriteDelta(BitWriter w) { for (int i = 0; i < bones.Count; i++) { w.WriteBool(_dirty[i]); if (_dirty[i]) { WriteBone(w, i); _dirty[i] = false; } } }
        public override void ReadDelta(BitReader r) { for (int i = 0; i < bones.Count; i++) if (r.ReadBool()) ReadBone(r, i); if (owner.smoothing <= 0) Apply(1f); }
    }
}
