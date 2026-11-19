// Replicates Animator parameters through a schema component (default "Animator"): every field of the
// component maps to the Animator parameter of the same name: f32/quantized → float, int/uint →
// integer, bool → bool. Triggers are not state: fire them with a [Rpc] on the owner.
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity
{
    [SyncComponent("Animator")]
    [RequireComponent(typeof(Animator))]
    public sealed class NetAnimator : SyncBehaviour
    {
        [Tooltip(
            "Remote copies blend float parameters towards the replicated value with this rate (0 = snap)."
        )]
        public float FloatDamping = 0f;
        Animator _anim;

        protected override void OnBound()
        {
            _anim = GetComponent<Animator>();
        }

        protected override bool CollectCustom(ComponentValue value)
        {
            if (_anim == null || NetComponent == null)
            {
                return false;
            }

            for (int i = 0; i < NetComponent.Fields.Count; i++)
            {
                var field = NetComponent.Fields[i];
                switch (field.Type.Kind)
                {
                    case FieldKind.Bool:
                        value.Fields[i] = new Value { B = _anim.GetBool(field.Name) };
                        break;
                    case FieldKind.UInt:
                        value.Fields[i] = new Value
                        {
                            U = (ulong)Mathf.Max(0, _anim.GetInteger(field.Name)),
                        };
                        break;
                    case FieldKind.Int:
                        value.Fields[i] = new Value { I = _anim.GetInteger(field.Name) };
                        break;
                    case FieldKind.F32:
                    case FieldKind.Quant:
                        value.Fields[i] = new Value { F = _anim.GetFloat(field.Name) };
                        break;
                    default:
                        break;
                }
            }
            return true;
        }

        protected override bool ApplyCustom(ComponentValue value)
        {
            if (_anim == null || NetComponent == null)
            {
                return false;
            }

            for (int i = 0; i < NetComponent.Fields.Count; i++)
            {
                var field = NetComponent.Fields[i];
                switch (field.Type.Kind)
                {
                    case FieldKind.Bool:
                        _anim.SetBool(field.Name, value.Fields[i].B);
                        break;
                    case FieldKind.UInt:
                        _anim.SetInteger(field.Name, (int)value.Fields[i].U);
                        break;
                    case FieldKind.Int:
                        _anim.SetInteger(field.Name, (int)value.Fields[i].I);
                        break;
                    case FieldKind.F32:
                    case FieldKind.Quant:
                        if (FloatDamping > 0f)
                        {
                            _anim.SetFloat(
                                field.Name,
                                value.Fields[i].F,
                                FloatDamping,
                                Time.unscaledDeltaTime
                            );
                        }
                        else
                        {
                            _anim.SetFloat(field.Name, value.Fields[i].F);
                        }

                        break;
                    default:
                        break;
                }
            }
            return true;
        }
    }
}
