// Replicates position/rotation through the schema's Transform component (pos: vec3, rot: quat).
// Owner: writes the transform every tick. Remote: interpolates between the last two server states.
using System.Collections.Generic;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity
{
    [SyncComponent("Transform")]
    public sealed class NetTransform : SyncBehaviour
    {
        [Sync("pos")] public Vector3 Position;
        [Sync("rot")] public Quaternion Rotation = Quaternion.identity;
        [Tooltip("Snap instead of interpolating when the remote jumped farther than this.")] public float TeleportDistance = 5f;

        struct Sample { public uint Tick; public Vector3 Pos; public Quaternion Rot; }
        readonly List<Sample> _buf = new List<Sample>(8);

        protected override void OnBound()
        {
            if (IsMine) { Position = transform.position; Rotation = transform.rotation; }
            else { transform.position = Position; transform.rotation = Rotation; _buf.Clear(); _buf.Add(new Sample { Tick = View.Runner.ServerTick, Pos = Position, Rot = Rotation }); }
        }

        protected override void OnSynced(uint tick)
        {
            if (_buf.Count > 0 && Vector3.Distance(_buf[_buf.Count - 1].Pos, Position) > TeleportDistance) _buf.Clear();
            _buf.Add(new Sample { Tick = tick, Pos = Position, Rot = Rotation });
            while (_buf.Count > 8) _buf.RemoveAt(0);
        }

        void Update()
        {
            if (View == null) return;
            if (IsMine) { Position = transform.position; Rotation = transform.rotation; return; }
            if (_buf.Count == 0) return;
            float render = View.Runner.RenderTick;
            Sample a = _buf[0], b = _buf[_buf.Count - 1];
            for (int i = 0; i + 1 < _buf.Count; i++) if (_buf[i].Tick <= render && _buf[i + 1].Tick >= render) { a = _buf[i]; b = _buf[i + 1]; break; }
            float t = b.Tick > a.Tick ? Mathf.Clamp01((render - a.Tick) / (b.Tick - a.Tick)) : 1f;
            if (render > b.Tick) t = 1f;
            transform.position = Vector3.Lerp(a.Pos, b.Pos, t);
            transform.rotation = Quaternion.Slerp(a.Rot, b.Rot, t);
        }
    }
}
