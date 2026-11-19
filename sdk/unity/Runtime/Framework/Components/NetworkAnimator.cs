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
        readonly List<bool> _dirtyF = new List<bool>(),
            _dirtyI = new List<bool>(),
            _dirtyB = new List<bool>();

        public AnimatorSync(bool ownerAuth = true)
        {
            this.ownerAuth = ownerAuth;
        }

        public void Resize(int floatCount, int intCount, int boolCount)
        {
            while (floats.Count < floatCount)
            {
                floats.Add(0);
                _dirtyF.Add(true);
            }
            while (ints.Count < intCount)
            {
                ints.Add(0);
                _dirtyI.Add(true);
            }
            while (bools.Count < boolCount)
            {
                bools.Add(false);
                _dirtyB.Add(true);
            }
        }

        public void SetFloat(int index, float value)
        {
            if (floats[index] != value)
            {
                floats[index] = value;
                _dirtyF[index] = true;
                isDirty = true;
            }
        }

        public void SetInt(int index, int value)
        {
            if (ints[index] != value)
            {
                ints[index] = value;
                _dirtyI[index] = true;
                isDirty = true;
            }
        }

        public void SetBool(int index, bool value)
        {
            if (bools[index] != value)
            {
                bools[index] = value;
                _dirtyB[index] = true;
                isDirty = true;
            }
        }

        public override void WriteFull(BitWriter writer)
        {
            writer.WriteUInt((ulong)floats.Count, 8);
            foreach (var value in floats)
            {
                writer.WriteF32(value);
            }

            writer.WriteUInt((ulong)ints.Count, 8);
            foreach (var index in ints)
            {
                writer.WriteBits((uint)index, 32);
            }

            writer.WriteUInt((ulong)bools.Count, 8);
            foreach (var value in bools)
            {
                writer.WriteBool(value);
            }
        }

        public override void ReadFull(BitReader reader)
        {
            int nf = (int)reader.ReadUInt(8),
                ni,
                nb;
            Resize(nf, 0, 0);
            for (int i = 0; i < nf; i++)
            {
                floats[i] = reader.ReadF32();
            }

            ni = (int)reader.ReadUInt(8);
            Resize(nf, ni, 0);
            for (int i = 0; i < ni; i++)
            {
                ints[i] = (int)(uint)reader.ReadBits(32);
            }

            nb = (int)reader.ReadUInt(8);
            Resize(nf, ni, nb);
            for (int i = 0; i < nb; i++)
            {
                bools[i] = reader.ReadBool();
            }
        }

        public override void WriteDelta(BitWriter writer)
        {
            for (int i = 0; i < floats.Count; i++)
            {
                writer.WriteBool(_dirtyF[i]);
                if (_dirtyF[i])
                {
                    writer.WriteF32(floats[i]);
                    _dirtyF[i] = false;
                }
            }
            for (int i = 0; i < ints.Count; i++)
            {
                writer.WriteBool(_dirtyI[i]);
                if (_dirtyI[i])
                {
                    writer.WriteBits((uint)ints[i], 32);
                    _dirtyI[i] = false;
                }
            }
            for (int i = 0; i < bools.Count; i++)
            {
                writer.WriteBool(_dirtyB[i]);
                if (_dirtyB[i])
                {
                    writer.WriteBool(bools[i]);
                    _dirtyB[i] = false;
                }
            }
        }

        public override void ReadDelta(BitReader reader)
        {
            for (int i = 0; i < floats.Count; i++)
            {
                if (reader.ReadBool())
                {
                    floats[i] = reader.ReadF32();
                }
            }

            for (int i = 0; i < ints.Count; i++)
            {
                if (reader.ReadBool())
                {
                    ints[i] = (int)(uint)reader.ReadBits(32);
                }
            }

            for (int i = 0; i < bools.Count; i++)
            {
                if (reader.ReadBool())
                {
                    bools[i] = reader.ReadBool();
                }
            }
        }
    }

    [RequireComponent(typeof(Animator))]
    public class NetworkAnimator : NetworkBehaviour
    {
        [Tooltip("Who drives the animator: the owner (true) or the server (false).")]
        public bool ownerAuth = true;

        [Tooltip(
            "Remote copies blend float parameters towards the replicated value with this rate (0 = snap)."
        )]
        public float floatDamping = 10f;

        public AnimatorSync sync;
        Animator _anim;
        readonly List<string> _floatNames = new List<string>(),
            _intNames = new List<string>(),
            _boolNames = new List<string>();

        void Awake() => EnsureSync();

        protected override void OnBeforeBind() => EnsureSync();

        void EnsureSync()
        {
            if (sync != null)
            {
                return;
            }

            sync = new AnimatorSync(ownerAuth);
            _anim = GetComponent<Animator>();
            if (_anim == null)
            {
                return;
            }

            foreach (var parameter in _anim.parameters)
            {
                switch (parameter.type)
                {
                    case AnimatorControllerParameterType.Float:
                        _floatNames.Add(parameter.name);
                        break;
                    case AnimatorControllerParameterType.Int:
                        _intNames.Add(parameter.name);
                        break;
                    case AnimatorControllerParameterType.Bool:
                        _boolNames.Add(parameter.name);
                        break;
                }
            }
            sync.Resize(_floatNames.Count, _intNames.Count, _boolNames.Count);
            Sample();
        }

        public override void OnTick(uint tick)
        {
            if (isController)
            {
                Sample();
            }
        }

        void Sample()
        {
            if (_anim == null)
            {
                return;
            }

            for (int i = 0; i < _floatNames.Count; i++)
            {
                sync.SetFloat(i, _anim.GetFloat(_floatNames[i]));
            }

            for (int i = 0; i < _intNames.Count; i++)
            {
                sync.SetInt(i, _anim.GetInteger(_intNames[i]));
            }

            for (int i = 0; i < _boolNames.Count; i++)
            {
                sync.SetBool(i, _anim.GetBool(_boolNames[i]));
            }
        }

        void Update()
        {
            if (!isSpawned || isController || _anim == null)
            {
                return;
            }

            for (int i = 0; i < _floatNames.Count && i < sync.floats.Count; i++)
            {
                _anim.SetFloat(
                    _floatNames[i],
                    floatDamping > 0
                        ? Mathf.Lerp(
                            _anim.GetFloat(_floatNames[i]),
                            sync.floats[i],
                            Mathf.Clamp01(floatDamping * Time.deltaTime)
                        )
                        : sync.floats[i]
                );
            }

            for (int i = 0; i < _intNames.Count && i < sync.ints.Count; i++)
            {
                _anim.SetInteger(_intNames[i], sync.ints[i]);
            }

            for (int i = 0; i < _boolNames.Count && i < sync.bools.Count; i++)
            {
                _anim.SetBool(_boolNames[i], sync.bools[i]);
            }
        }

        /// <summary>Controller: fire a trigger here and on every remote copy.</summary>
        public void SetTrigger(string name)
        {
            if (!isSpawned || !isController)
            {
                return;
            }

            if (_anim != null)
            {
                _anim.SetTrigger(name);
            }

            if (isServer)
            {
                Rpc(nameof(RpcTrigger), name);
            }
            else
            {
                Rpc(nameof(CmdTrigger), name);
            }
        }

        [ServerRpc(requireOwnership: true)]
        void CmdTrigger(string name)
        {
            if (_anim != null && !isController)
            {
                _anim.SetTrigger(name);
            }

            Rpc(nameof(RpcTrigger), name);
        }

        [ObserversRpc(runLocally: false, excludeOwner: true)]
        void RpcTrigger(string name)
        {
            if (_anim != null && !isController)
            {
                _anim.SetTrigger(name);
            }
        }
    }
}
