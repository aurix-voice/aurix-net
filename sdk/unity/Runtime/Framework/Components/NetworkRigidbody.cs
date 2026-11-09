// NetworkTransform for a Rigidbody: the controller simulates and also sends velocities; remote
// copies are kinematic, interpolate like NetworkTransform and extrapolate with the last velocity
// while the next sample has not arrived.
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    [RequireComponent(typeof(Rigidbody))]
    public class NetworkRigidbody : NetworkTransform
    {
        public SyncVar<Vector3> velocity = new SyncVar<Vector3>(default, 0f, ownerAuth: true);
        public SyncVar<Vector3> angularVelocity = new SyncVar<Vector3>(default, 0f, ownerAuth: true);
        [Tooltip("Remote copies extrapolate with the last velocity for at most this many ticks.")] public float maxExtrapolationTicks = 3f;

        Rigidbody _rb;

        protected override void Awake() { base.Awake(); _rb = GetComponent<Rigidbody>(); }
        protected override void OnBeforeBind() { base.OnBeforeBind(); if (_rb == null) _rb = GetComponent<Rigidbody>(); }

        protected override void OnSpawned(bool asServer)
        {
            if (_rb == null) _rb = GetComponent<Rigidbody>();
            base.OnSpawned(asServer);
            if (!isController && _rb != null) _rb.isKinematic = true;
        }

        protected override void OnOwnerChanged(PlayerID? o, PlayerID? n)
        {
            if (_rb != null && isSpawned) _rb.isKinematic = !isController;
        }

        public override void OnTick(uint tick)
        {
            base.OnTick(tick);
            if (isController && _rb != null) { velocity.value = _rb.velocity; angularVelocity.value = _rb.angularVelocity; }
        }

        protected override void Apply(Vector3 pos, Quaternion rot, Vector3 scale)
        {
            if (_rb == null) { base.Apply(pos, rot, scale); return; }
            var buf = sync.buffer;
            if (buf.Count > 0 && networkManager != null)
            {
                var last = buf[buf.Count - 1];
                float behind = (networkManager.serverTick - interpolationTicks) - last.tick;
                if (behind > 0) pos = last.pos + velocity.value * (Mathf.Min(behind, maxExtrapolationTicks) / Mathf.Max(1, networkManager.tickRate));
            }
            _rb.MovePosition(pos); _rb.MoveRotation(rot);
        }
    }
}
