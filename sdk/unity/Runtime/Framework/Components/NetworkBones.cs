// Replicate skeleton bones (local pose) for ragdolls / procedural animation.
// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    /// <summary>
    /// Replicates the local position/rotation/scale of every bone gathered from the SkinnedMeshRenderers under this
    /// object plus <see cref="extraBones"/>. Only bones that moved beyond the accuracy thresholds are sent; remotes
    /// smooth toward the latest pose. Set bones before spawn.
    /// </summary>
    public class NetworkBones : NetworkBehaviour
    {
        [Tooltip("Owner samples and sends (otherwise the server).")]
        public bool ownerAuth = true;

        [Tooltip("Max sends per second.")]
        public float sendRate = 10f;

        [Tooltip("Extra bones to sync on top of the ones gathered from SkinnedMeshRenderers.")]
        public Transform[] extraBones = Array.Empty<Transform>();
        public bool syncPosition = true,
            syncRotation = true,
            syncScale;
        public float positionAccuracy = 0.01f,
            angleAccuracy = 0.5f,
            scaleAccuracy = 0.05f,
            positionRange = 64f;

        [Tooltip("Remote smoothing speed (1/s); 0 snaps.")]
        public float smoothing = 20f;

        BonesSync _sync;
        public int boneCount => _sync?.bones.Count ?? 0;
        public IReadOnlyList<Transform> bones => _sync?.bones;

        protected override void OnBeforeBind()
        {
            _sync = new BonesSync(ownerAuth, sendRate > 0 ? 1f / sendRate : 0f) { owner = this };
            var list = new List<Transform>();
            foreach (var smr in GetComponentsInChildren<SkinnedMeshRenderer>())
            {
                if (smr.bones != null)
                {
                    foreach (var bone in smr.bones)
                    {
                        if (bone != null && !list.Contains(bone))
                        {
                            list.Add(bone);
                        }
                    }
                }
            }

            foreach (var bone in extraBones)
            {
                if (bone != null && !list.Contains(bone))
                {
                    list.Add(bone);
                }
            }

            _sync.Configure(list);
        }

        public override void OnTick(uint tick)
        {
            if (isController)
            {
                _sync.Sample();
            }
        }

        protected virtual void LateUpdate() => RenderUpdate(Time.deltaTime);

        /// <summary>Remote side: ease bones toward the last received pose.</summary>
        public void RenderUpdate(float dt)
        {
            if (isSpawned && !isController)
            {
                _sync.Apply(smoothing > 0 ? Math.Min(1f, dt * smoothing) : 1f);
            }
        }
    }

    public sealed class BonesSync : NetworkModule
    {
        internal NetworkBones owner;
        public readonly List<Transform> bones = new List<Transform>();
        Vector3[] _pos = Array.Empty<Vector3>(),
            _scale = Array.Empty<Vector3>(),
            _tPos = Array.Empty<Vector3>(),
            _tScale = Array.Empty<Vector3>();
        Quaternion[] _rot = Array.Empty<Quaternion>(),
            _tRot = Array.Empty<Quaternion>();
        bool[] _dirty = Array.Empty<bool>();

        public BonesSync(bool ownerAuth, float sendInterval)
        {
            this.ownerAuth = ownerAuth;
            sendIntervalInSeconds = sendInterval;
        }

        public void Configure(List<Transform> list)
        {
            bones.Clear();
            bones.AddRange(list);
            int count = bones.Count;
            _pos = new Vector3[count];
            _scale = new Vector3[count];
            _rot = new Quaternion[count];
            _tPos = new Vector3[count];
            _tScale = new Vector3[count];
            _tRot = new Quaternion[count];
            _dirty = new bool[count];
            for (int i = 0; i < count; i++)
            {
                _pos[i] = _tPos[i] = bones[i].localPosition;
                _rot[i] = _tRot[i] = bones[i].localRotation;
                _scale[i] = _tScale[i] = bones[i].localScale;
            }
        }

        public void Sample()
        {
            for (int i = 0; i < bones.Count; i++)
            {
                var bone = bones[i];
                bool moved = false;
                if (
                    owner.syncPosition
                    && Vector3.Distance(bone.localPosition, _pos[i]) > owner.positionAccuracy
                )
                {
                    _pos[i] = bone.localPosition;
                    moved = true;
                }
                if (
                    owner.syncRotation
                    && Quaternion.Angle(bone.localRotation, _rot[i]) > owner.angleAccuracy
                )
                {
                    _rot[i] = bone.localRotation;
                    moved = true;
                }
                if (
                    owner.syncScale
                    && Vector3.Distance(bone.localScale, _scale[i]) > owner.scaleAccuracy
                )
                {
                    _scale[i] = bone.localScale;
                    moved = true;
                }
                if (moved)
                {
                    _dirty[i] = true;
                    isDirty = true;
                }
            }
        }

        public void Apply(float fraction)
        {
            for (int i = 0; i < bones.Count; i++)
            {
                var bone = bones[i];
                if (owner.syncPosition)
                {
                    bone.localPosition = Vector3.Lerp(bone.localPosition, _tPos[i], fraction);
                }

                if (owner.syncRotation)
                {
                    bone.localRotation = Quaternion.Slerp(bone.localRotation, _tRot[i], fraction);
                }

                if (owner.syncScale)
                {
                    bone.localScale = Vector3.Lerp(bone.localScale, _tScale[i], fraction);
                }
            }
        }

        void WriteBone(BitWriter writer, int index)
        {
            if (owner.syncPosition)
            {
                float range = owner.positionRange,
                    accuracy = owner.positionAccuracy;
                writer.WriteQuantized(_pos[index].x, -range, range, accuracy);
                writer.WriteQuantized(_pos[index].y, -range, range, accuracy);
                writer.WriteQuantized(_pos[index].z, -range, range, accuracy);
            }
            if (owner.syncRotation)
            {
                TransformSync.WriteQuat(writer, _rot[index]);
            }

            if (owner.syncScale)
            {
                writer.WriteF32(_scale[index].x);
                writer.WriteF32(_scale[index].y);
                writer.WriteF32(_scale[index].z);
            }
        }

        void ReadBone(BitReader reader, int index)
        {
            if (owner.syncPosition)
            {
                float rg = owner.positionRange,
                    accuracy = owner.positionAccuracy;
                _tPos[index] = new Vector3(
                    reader.ReadQuantized(-rg, rg, accuracy),
                    reader.ReadQuantized(-rg, rg, accuracy),
                    reader.ReadQuantized(-rg, rg, accuracy)
                );
            }
            if (owner.syncRotation)
            {
                _tRot[index] = TransformSync.ReadQuat(reader);
            }

            if (owner.syncScale)
            {
                _tScale[index] = new Vector3(reader.ReadF32(), reader.ReadF32(), reader.ReadF32());
            }
        }

        public override void WriteFull(BitWriter writer)
        {
            writer.WriteUInt((ulong)bones.Count, 16);
            for (int i = 0; i < bones.Count; i++)
            {
                WriteBone(writer, i);
            }
        }

        public override void ReadFull(BitReader reader)
        {
            int count = (int)reader.ReadUInt(16);
            if (count != bones.Count)
            {
                Debug.LogError(
                    $"NetworkBones: {count} bones received, {bones.Count} local; skipping"
                );
                return;
            }
            for (int i = 0; i < count; i++)
            {
                ReadBone(reader, i);
            }

            Apply(1f);
        }

        public override void WriteDelta(BitWriter writer)
        {
            for (int i = 0; i < bones.Count; i++)
            {
                writer.WriteBool(_dirty[i]);
                if (_dirty[i])
                {
                    WriteBone(writer, i);
                    _dirty[i] = false;
                }
            }
        }

        public override void ReadDelta(BitReader reader)
        {
            for (int i = 0; i < bones.Count; i++)
            {
                if (reader.ReadBool())
                {
                    ReadBone(reader, i);
                }
            }

            if (owner.smoothing <= 0)
            {
                Apply(1f);
            }
        }
    }
}
