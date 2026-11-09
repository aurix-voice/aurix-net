// Replicates Animator parameters (float / int / bool) from the controller and forwards triggers
// as RPCs. Remote copies blend floats towards the replicated value.
using System.Collections.Generic;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    public sealed class AnimatorSync : NetworkModule
    {
        public readonly List<float> floats = new List<float>();
        public readonly List<int> ints = new List<int>();
        public readonly List<bool> bools = new List<bool>();
        readonly List<bool> _dirtyF = new List<bool>(), _dirtyI = new List<bool>(), _dirtyB = new List<bool>();

        public AnimatorSync(bool ownerAuth = true) { this.ownerAuth = ownerAuth; }

        public void Resize(int f, int i, int b)
        {
            while (floats.Count < f) { floats.Add(0); _dirtyF.Add(true); } while (ints.Count < i) { ints.Add(0); _dirtyI.Add(true); } while (bools.Count < b) { bools.Add(false); _dirtyB.Add(true); }
        }
        public void SetFloat(int i, float v) { if (floats[i] != v) { floats[i] = v; _dirtyF[i] = true; isDirty = true; } }
        public void SetInt(int i, int v) { if (ints[i] != v) { ints[i] = v; _dirtyI[i] = true; isDirty = true; } }
        public void SetBool(int i, bool v) { if (bools[i] != v) { bools[i] = v; _dirtyB[i] = true; isDirty = true; } }

        public override void WriteFull(BitWriter w)
        {
            w.WriteUInt((ulong)floats.Count, 8); foreach (var f in floats) w.WriteF32(f);
            w.WriteUInt((ulong)ints.Count, 8); foreach (var i in ints) w.WriteBits((uint)i, 32);
            w.WriteUInt((ulong)bools.Count, 8); foreach (var b in bools) w.WriteBool(b);
        }
        public override void ReadFull(BitReader r)
        {
            int nf = (int)r.ReadUInt(8), ni, nb; Resize(nf, 0, 0); for (int i = 0; i < nf; i++) floats[i] = r.ReadF32();
            ni = (int)r.ReadUInt(8); Resize(nf, ni, 0); for (int i = 0; i < ni; i++) ints[i] = (int)(uint)r.ReadBits(32);
            nb = (int)r.ReadUInt(8); Resize(nf, ni, nb); for (int i = 0; i < nb; i++) bools[i] = r.ReadBool();
        }
        public override void WriteDelta(BitWriter w)
        {
            for (int i = 0; i < floats.Count; i++) { w.WriteBool(_dirtyF[i]); if (_dirtyF[i]) { w.WriteF32(floats[i]); _dirtyF[i] = false; } }
            for (int i = 0; i < ints.Count; i++) { w.WriteBool(_dirtyI[i]); if (_dirtyI[i]) { w.WriteBits((uint)ints[i], 32); _dirtyI[i] = false; } }
            for (int i = 0; i < bools.Count; i++) { w.WriteBool(_dirtyB[i]); if (_dirtyB[i]) { w.WriteBool(bools[i]); _dirtyB[i] = false; } }
        }
        public override void ReadDelta(BitReader r)
        {
            for (int i = 0; i < floats.Count; i++) if (r.ReadBool()) floats[i] = r.ReadF32();
            for (int i = 0; i < ints.Count; i++) if (r.ReadBool()) ints[i] = (int)(uint)r.ReadBits(32);
            for (int i = 0; i < bools.Count; i++) if (r.ReadBool()) bools[i] = r.ReadBool();
        }
    }

    [RequireComponent(typeof(Animator))]
    public class NetworkAnimator : NetworkBehaviour
    {
        [Tooltip("Who drives the animator: the owner (true) or the server (false).")] public bool ownerAuth = true;
        [Tooltip("Remote copies blend float parameters towards the replicated value with this rate (0 = snap).")] public float floatDamping = 10f;

        public AnimatorSync sync;
        Animator _anim;
        readonly List<string> _floatNames = new List<string>(), _intNames = new List<string>(), _boolNames = new List<string>();

        void Awake() => EnsureSync();
        protected override void OnBeforeBind() => EnsureSync();

        void EnsureSync()
        {
            if (sync != null) return;
            sync = new AnimatorSync(ownerAuth);
            _anim = GetComponent<Animator>();
            if (_anim == null) return;
            foreach (var p in _anim.parameters)
            {
                switch (p.type)
                {
                    case AnimatorControllerParameterType.Float: _floatNames.Add(p.name); break;
                    case AnimatorControllerParameterType.Int: _intNames.Add(p.name); break;
                    case AnimatorControllerParameterType.Bool: _boolNames.Add(p.name); break;
                }
            }
            sync.Resize(_floatNames.Count, _intNames.Count, _boolNames.Count);
            Sample();
        }

        public override void OnTick(uint tick) { if (isController) Sample(); }

        void Sample()
        {
            if (_anim == null) return;
            for (int i = 0; i < _floatNames.Count; i++) sync.SetFloat(i, _anim.GetFloat(_floatNames[i]));
            for (int i = 0; i < _intNames.Count; i++) sync.SetInt(i, _anim.GetInteger(_intNames[i]));
            for (int i = 0; i < _boolNames.Count; i++) sync.SetBool(i, _anim.GetBool(_boolNames[i]));
        }

        void Update()
        {
            if (!isSpawned || isController || _anim == null) return;
            for (int i = 0; i < _floatNames.Count && i < sync.floats.Count; i++)
                _anim.SetFloat(_floatNames[i], floatDamping > 0 ? Mathf.Lerp(_anim.GetFloat(_floatNames[i]), sync.floats[i], Mathf.Clamp01(floatDamping * Time.deltaTime)) : sync.floats[i]);
            for (int i = 0; i < _intNames.Count && i < sync.ints.Count; i++) _anim.SetInteger(_intNames[i], sync.ints[i]);
            for (int i = 0; i < _boolNames.Count && i < sync.bools.Count; i++) _anim.SetBool(_boolNames[i], sync.bools[i]);
        }

        /// <summary>Controller: fire a trigger here and on every remote copy.</summary>
        public void SetTrigger(string name)
        {
            if (!isSpawned || !isController) return;
            if (_anim != null) _anim.SetTrigger(name);
            if (isServer) Rpc(nameof(RpcTrigger), name); else Rpc(nameof(CmdTrigger), name);
        }

        [ServerRpc(requireOwnership: true)] void CmdTrigger(string name) { if (_anim != null && !isController) _anim.SetTrigger(name); Rpc(nameof(RpcTrigger), name); }
        [ObserversRpc(runLocally: false, excludeOwner: true)] void RpcTrigger(string name) { if (_anim != null && !isController) _anim.SetTrigger(name); }
    }
}
