// Lag compensation (PurrNet ColliderRollback-style): the server keeps a per-tick history of this object's
// pose; NetworkManager.Rollback(shooter, body) rewinds every ColliderRollback to the tick the shooter was
// looking at (its reported view tick), runs body (raycasts, overlaps…), then restores the live pose.
using System;
using System.Collections.Generic;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    public class ColliderRollback : NetworkBehaviour
    {
        public struct Pose { public uint tick; public Vector3 pos; public Quaternion rot; }

        [Tooltip("How many seconds of poses the server keeps.")] public float historySeconds = 1f;
        readonly List<Pose> _history = new List<Pose>(64);
        Pose _live; bool _rolledBack;

        public IReadOnlyList<Pose> history => _history;
        public bool isRolledBack => _rolledBack;

        public override void OnTick(uint tick)
        {
            if (!isServer || _rolledBack) return;
            int cap = Math.Max(2, (int)(historySeconds * Math.Max(1, networkManager.tickRate)));
            if (_history.Count >= cap) _history.RemoveAt(0);
            _history.Add(new Pose { tick = tick, pos = transform.position, rot = transform.rotation });
        }

        /// <summary>Pose at the given server tick, interpolated between the two nearest samples; clamped to the history range.</summary>
        public bool TryGetPose(uint tick, out Pose pose)
        {
            pose = default;
            if (_history.Count == 0) return false;
            if (tick <= _history[0].tick) { pose = _history[0]; pose.tick = tick; return true; }
            var last = _history[_history.Count - 1];
            if (tick >= last.tick) { pose = last; pose.tick = tick; return true; }
            for (int i = 1; i < _history.Count; i++)
            {
                if (_history[i].tick < tick) continue;
                var a = _history[i - 1]; var b = _history[i];
                float t = (tick - a.tick) / (float)Math.Max(1, b.tick - a.tick);
                pose = new Pose { tick = tick, pos = Vector3.Lerp(a.pos, b.pos, t), rot = Quaternion.Slerp(a.rot, b.rot, t) };
                return true;
            }
            return false;
        }

        internal void Rollback(uint tick)
        {
            if (!TryGetPose(tick, out var p)) return;
            if (!_rolledBack) { _live = new Pose { pos = transform.position, rot = transform.rotation }; _rolledBack = true; }
            transform.position = p.pos; transform.rotation = p.rot;
        }

        internal void Restore()
        {
            if (!_rolledBack) return;
            transform.position = _live.pos; transform.rotation = _live.rot; _rolledBack = false;
        }
    }
}
