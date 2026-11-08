// Replicates a Rigidbody through a schema component (default "Rigidbody": pos vec3, rot quat, vel vec3,
// ang vec3). Owner: simulates and sends every tick. Remote: kinematic, interpolated like NetTransform
// with velocity-based extrapolation when the next sample has not arrived yet.
using System.Collections.Generic;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity
{
    [SyncComponent("Rigidbody")]
    [RequireComponent(typeof(Rigidbody))]
    public sealed class NetRigidbody : SyncBehaviour
    {
        [Sync("pos")] public Vector3 Position;
        [Sync("rot")] public Quaternion Rotation = Quaternion.identity;
        [Sync("vel")] public Vector3 Velocity;
        [Sync("ang")] public Vector3 AngularVelocity;
        [Tooltip("Snap instead of interpolating when the remote jumped farther than this.")] public float TeleportDistance = 5f;
        [Tooltip("Remote copies extrapolate with the last velocity for at most this many ticks.")] public float MaxExtrapolationTicks = 3f;

        struct Sample { public uint Tick; public Vector3 Pos; public Quaternion Rot; public Vector3 Vel; }
        readonly List<Sample> _buf = new List<Sample>(8);
        Rigidbody _rb;

        protected override void OnBound()
        {
            _rb = GetComponent<Rigidbody>();
            if (IsMine) { Position = _rb.position; Rotation = _rb.rotation; Velocity = _rb.velocity; AngularVelocity = _rb.angularVelocity; }
            else { _rb.isKinematic = true; _rb.position = Position; _rb.rotation = Rotation; _buf.Clear(); _buf.Add(new Sample { Tick = View.Runner.ServerTick, Pos = Position, Rot = Rotation, Vel = Velocity }); }
        }

        protected override void OnBeforeSend()
        {
            if (_rb == null) return;
            Position = _rb.position; Rotation = _rb.rotation; Velocity = _rb.velocity; AngularVelocity = _rb.angularVelocity;
        }

        protected override void OnSynced(uint tick)
        {
            if (_buf.Count > 0 && Vector3.Distance(_buf[_buf.Count - 1].Pos, Position) > TeleportDistance) _buf.Clear();
            _buf.Add(new Sample { Tick = tick, Pos = Position, Rot = Rotation, Vel = Velocity });
            while (_buf.Count > 8) _buf.RemoveAt(0);
        }

        void Update()
        {
            if (View == null || IsMine || _rb == null || _buf.Count == 0) return;
            float render = View.Runner.RenderTick; float hz = Mathf.Max(1, View.Runner.Settings.TickHz);
            Sample a = _buf[0], b = _buf[_buf.Count - 1];
            for (int i = 0; i + 1 < _buf.Count; i++) if (_buf[i].Tick <= render && _buf[i + 1].Tick >= render) { a = _buf[i]; b = _buf[i + 1]; break; }
            Vector3 pos; Quaternion rot;
            if (render > b.Tick)
            {
                float ahead = Mathf.Min(render - b.Tick, MaxExtrapolationTicks);
                pos = b.Pos + b.Vel * (ahead / hz); rot = b.Rot;
            }
            else
            {
                float t = b.Tick > a.Tick ? Mathf.Clamp01((render - a.Tick) / (b.Tick - a.Tick)) : 1f;
                pos = Vector3.Lerp(a.Pos, b.Pos, t); rot = Quaternion.Slerp(a.Rot, b.Rot, t);
            }
            _rb.MovePosition(pos); _rb.MoveRotation(rot);
        }
    }
}
