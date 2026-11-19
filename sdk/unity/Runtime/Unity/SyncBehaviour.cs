// Base class for replicated behaviours: fields marked [Sync] map onto a schema component's
// fields by name (component = [SyncComponent("Name")] or the class name). The owner sends
// changed values every tick as its input; everyone else receives them from server deltas.
// [Rpc] methods can be called through NetView.Rpc("Method", args...).
using System;
using System.Collections.Generic;
using System.Reflection;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity
{
    [AttributeUsage(AttributeTargets.Field)]
    public sealed class SyncAttribute : Attribute
    {
        public string Name;

        public SyncAttribute(string name = null)
        {
            Name = name;
        }
    }

    [AttributeUsage(AttributeTargets.Class)]
    public sealed class SyncComponentAttribute : Attribute
    {
        public string Name;

        public SyncComponentAttribute(string name)
        {
            Name = name;
        }
    }

    [AttributeUsage(AttributeTargets.Method)]
    public sealed class RpcAttribute : Attribute { }

    public abstract class SyncBehaviour : MonoBehaviour
    {
        public NetView View { get; private set; }
        public bool IsMine => View != null && View.IsMine;
        protected ComponentDef NetComponent { get; private set; }
        FieldInfo[] _fields;
        int[] _slots;
        ComponentValue _lastSent;
        readonly Dictionary<string, MethodInfo> _rpcs = new Dictionary<string, MethodInfo>();

        internal void Bind(NetView view)
        {
            View = view;
            var type = GetType();
            var cname = type.GetCustomAttribute<SyncComponentAttribute>()?.Name ?? type.Name;
            NetComponent = view.Runner.Schema?.ByName(cname);
            var fs = new List<FieldInfo>();
            var slots = new List<int>();
            foreach (
                var field in type.GetFields(
                    BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic
                )
            )
            {
                var attribute = field.GetCustomAttribute<SyncAttribute>();
                if (attribute == null)
                {
                    continue;
                }

                int idx = NetComponent?.IndexOf(attribute.Name ?? field.Name) ?? -1;
                if (idx < 0)
                {
                    Debug.LogWarning(
                        $"{type.Name}.{field.Name}: no field '{attribute.Name ?? field.Name}' in component '{cname}' of the schema"
                    );
                    continue;
                }
                fs.Add(field);
                slots.Add(idx);
            }
            _fields = fs.ToArray();
            _slots = slots.ToArray();
            foreach (
                var method in type.GetMethods(
                    BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic
                )
            )
            {
                if (method.GetCustomAttribute<RpcAttribute>() != null)
                {
                    _rpcs[method.Name] = method;
                }
            }

            if (!IsMine)
            {
                ApplyFromEntity(view.Entity, view.Runner.ServerTick);
            }

            OnBound();
        }

        /// <summary>Called once the view is bound (entity known, IsMine valid).</summary>
        protected virtual void OnBound() { }

        /// <summary>Called on remote copies after [Sync] fields were updated from a delta.</summary>
        protected virtual void OnSynced(uint tick) { }

        /// <summary>Owner side, before [Sync] fields are collected: copy engine state into them.</summary>
        protected virtual void OnBeforeSend() { }

        /// <summary>Schema-driven behaviours: fill <paramref name="value"/> yourself and return true to skip [Sync] fields.</summary>
        protected virtual bool CollectCustom(ComponentValue value) => false;

        /// <summary>Schema-driven behaviours: read <paramref name="value"/> yourself and return true to skip [Sync] fields.</summary>
        protected virtual bool ApplyCustom(ComponentValue value) => false;

        internal void ApplyFromEntity(NetEntity entity, uint tick)
        {
            if (NetComponent == null)
            {
                return;
            }

            var value = entity.Get(NetComponent);
            if (value == null)
            {
                return;
            }

            if (!ApplyCustom(value) && _fields != null)
            {
                for (int i = 0; i < _fields.Length; i++)
                {
                    _fields[i]
                        .SetValue(
                            this,
                            FromValue(
                                value.Fields[_slots[i]],
                                NetComponent.Fields[_slots[i]].Type,
                                _fields[i].FieldType
                            )
                        );
                }
            }

            OnSynced(tick);
        }

        /// <summary>Owner side: the component value to send this tick, or null when nothing changed.</summary>
        internal (ComponentDef, ComponentValue)? CollectWrite(bool force)
        {
            if (NetComponent == null)
            {
                return null;
            }

            OnBeforeSend();
            var value = _lastSent?.Clone() ?? ComponentValue.Zero(NetComponent);
            if (!CollectCustom(value))
            {
                if (_fields == null || _fields.Length == 0)
                {
                    return null;
                }

                for (int i = 0; i < _fields.Length; i++)
                {
                    value.Fields[_slots[i]] = ToValue(
                        _fields[i].GetValue(this),
                        NetComponent.Fields[_slots[i]].Type
                    );
                }
            }
            bool changed = force || _lastSent == null;
            if (!changed)
            {
                for (int i = 0; i < value.Fields.Length && !changed; i++)
                {
                    changed = !value.Fields[i].Equals(_lastSent.Fields[i]);
                }
            }

            if (!changed)
            {
                return null;
            }

            _lastSent = value;
            return (NetComponent, value);
        }

        internal bool InvokeRpc(string name, object[] args)
        {
            if (!_rpcs.TryGetValue(name, out var method))
            {
                return false;
            }

            method.Invoke(this, args);
            return true;
        }

        internal MethodInfo RpcMethod(string name) =>
            _rpcs.TryGetValue(name, out var method) ? method : null;

        static Value ToValue(object target, FieldType type)
        {
            switch (type.Kind)
            {
                case FieldKind.Bool:
                    return new Value { B = (bool)target };
                case FieldKind.UInt:
                    return new Value { U = Convert.ToUInt64(target) };
                case FieldKind.Int:
                    return new Value { I = Convert.ToInt64(target) };
                case FieldKind.F32:
                case FieldKind.Quant:
                    return new Value { F = Convert.ToSingle(target) };
                case FieldKind.Vec2:
                {
                    var vector = (Vector2)target;
                    return new Value { X = vector.x, Y = vector.y };
                }
                case FieldKind.Vec3:
                {
                    var vector = (Vector3)target;
                    return new Value
                    {
                        X = vector.x,
                        Y = vector.y,
                        Z = vector.z,
                    };
                }
                case FieldKind.Quat:
                {
                    var rotation = (Quaternion)target;
                    return new Value
                    {
                        X = rotation.x,
                        Y = rotation.y,
                        Z = rotation.z,
                        W = rotation.w,
                    };
                }
                case FieldKind.String:
                    return new Value { S = (string)target ?? "" };
                case FieldKind.Bytes:
                    return new Value { Bytes = (byte[])target ?? Array.Empty<byte>() };
            }
            return default;
        }

        static object FromValue(in Value value, FieldType type, Type target)
        {
            switch (type.Kind)
            {
                case FieldKind.Bool:
                    return value.B;
                case FieldKind.UInt:
                    return Convert.ChangeType(value.U, target);
                case FieldKind.Int:
                    return Convert.ChangeType(value.I, target);
                case FieldKind.F32:
                case FieldKind.Quant:
                    return Convert.ChangeType(value.F, target);
                case FieldKind.Vec2:
                    return new Vector2(value.X, value.Y);
                case FieldKind.Vec3:
                    return new Vector3(value.X, value.Y, value.Z);
                case FieldKind.Quat:
                    return new Quaternion(value.X, value.Y, value.Z, value.W);
                case FieldKind.String:
                    return value.S;
                case FieldKind.Bytes:
                    return value.Bytes;
            }
            return null;
        }
    }
}
