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
    [AttributeUsage(AttributeTargets.Field)] public sealed class SyncAttribute : Attribute { public string Name; public SyncAttribute(string name = null) { Name = name; } }
    [AttributeUsage(AttributeTargets.Class)] public sealed class SyncComponentAttribute : Attribute { public string Name; public SyncComponentAttribute(string name) { Name = name; } }
    [AttributeUsage(AttributeTargets.Method)] public sealed class RpcAttribute : Attribute { }

    public abstract class SyncBehaviour : MonoBehaviour
    {
        public NetView View { get; private set; }
        public bool IsMine => View != null && View.IsMine;
        protected ComponentDef NetComponent { get; private set; }
        FieldInfo[] _fields; int[] _slots; ComponentValue _lastSent;
        readonly Dictionary<string, MethodInfo> _rpcs = new Dictionary<string, MethodInfo>();

        internal void Bind(NetView view)
        {
            View = view;
            var t = GetType();
            var cname = t.GetCustomAttribute<SyncComponentAttribute>()?.Name ?? t.Name;
            NetComponent = view.Runner.Schema?.ByName(cname);
            var fs = new List<FieldInfo>(); var slots = new List<int>();
            foreach (var f in t.GetFields(BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic))
            {
                var a = f.GetCustomAttribute<SyncAttribute>(); if (a == null) continue;
                int idx = NetComponent?.IndexOf(a.Name ?? f.Name) ?? -1;
                if (idx < 0) { Debug.LogWarning($"{t.Name}.{f.Name}: no field '{a.Name ?? f.Name}' in component '{cname}' of the schema"); continue; }
                fs.Add(f); slots.Add(idx);
            }
            _fields = fs.ToArray(); _slots = slots.ToArray();
            foreach (var m in t.GetMethods(BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic)) if (m.GetCustomAttribute<RpcAttribute>() != null) _rpcs[m.Name] = m;
            if (!IsMine) ApplyFromEntity(view.Entity, view.Runner.ServerTick);
            OnBound();
        }

        /// <summary>Called once the view is bound (entity known, IsMine valid).</summary>
        protected virtual void OnBound() { }
        /// <summary>Called on remote copies after [Sync] fields were updated from a delta.</summary>
        protected virtual void OnSynced(uint tick) { }

        /// <summary>Owner side, before [Sync] fields are collected: copy engine state into them.</summary>
        protected virtual void OnBeforeSend() { }
        /// <summary>Schema-driven behaviours: fill <paramref name="v"/> yourself and return true to skip [Sync] fields.</summary>
        protected virtual bool CollectCustom(ComponentValue v) => false;
        /// <summary>Schema-driven behaviours: read <paramref name="v"/> yourself and return true to skip [Sync] fields.</summary>
        protected virtual bool ApplyCustom(ComponentValue v) => false;

        internal void ApplyFromEntity(NetEntity e, uint tick)
        {
            if (NetComponent == null) return;
            var v = e.Get(NetComponent); if (v == null) return;
            if (!ApplyCustom(v) && _fields != null)
                for (int i = 0; i < _fields.Length; i++) _fields[i].SetValue(this, FromValue(v.Fields[_slots[i]], NetComponent.Fields[_slots[i]].Type, _fields[i].FieldType));
            OnSynced(tick);
        }

        /// <summary>Owner side: the component value to send this tick, or null when nothing changed.</summary>
        internal (ComponentDef, ComponentValue)? CollectWrite(bool force)
        {
            if (NetComponent == null) return null;
            OnBeforeSend();
            var v = _lastSent?.Clone() ?? ComponentValue.Zero(NetComponent);
            if (!CollectCustom(v))
            {
                if (_fields == null || _fields.Length == 0) return null;
                for (int i = 0; i < _fields.Length; i++) v.Fields[_slots[i]] = ToValue(_fields[i].GetValue(this), NetComponent.Fields[_slots[i]].Type);
            }
            bool changed = force || _lastSent == null; if (!changed) for (int i = 0; i < v.Fields.Length && !changed; i++) changed = !v.Fields[i].Equals(_lastSent.Fields[i]);
            if (!changed) return null;
            _lastSent = v; return (NetComponent, v);
        }

        internal bool InvokeRpc(string name, object[] args) { if (!_rpcs.TryGetValue(name, out var m)) return false; m.Invoke(this, args); return true; }
        internal MethodInfo RpcMethod(string name) => _rpcs.TryGetValue(name, out var m) ? m : null;

        static Value ToValue(object o, FieldType t)
        {
            switch (t.Kind)
            {
                case FieldKind.Bool: return new Value { B = (bool)o };
                case FieldKind.UInt: return new Value { U = Convert.ToUInt64(o) };
                case FieldKind.Int: return new Value { I = Convert.ToInt64(o) };
                case FieldKind.F32: case FieldKind.Quant: return new Value { F = Convert.ToSingle(o) };
                case FieldKind.Vec2: { var v = (Vector2)o; return new Value { X = v.x, Y = v.y }; }
                case FieldKind.Vec3: { var v = (Vector3)o; return new Value { X = v.x, Y = v.y, Z = v.z }; }
                case FieldKind.Quat: { var q = (Quaternion)o; return new Value { X = q.x, Y = q.y, Z = q.z, W = q.w }; }
                case FieldKind.String: return new Value { S = (string)o ?? "" };
                case FieldKind.Bytes: return new Value { Bytes = (byte[])o ?? Array.Empty<byte>() };
            }
            return default;
        }
        static object FromValue(in Value v, FieldType t, Type target)
        {
            switch (t.Kind)
            {
                case FieldKind.Bool: return v.B;
                case FieldKind.UInt: return Convert.ChangeType(v.U, target);
                case FieldKind.Int: return Convert.ChangeType(v.I, target);
                case FieldKind.F32: case FieldKind.Quant: return Convert.ChangeType(v.F, target);
                case FieldKind.Vec2: return new Vector2(v.X, v.Y);
                case FieldKind.Vec3: return new Vector3(v.X, v.Y, v.Z);
                case FieldKind.Quat: return new Quaternion(v.X, v.Y, v.Z, v.W);
                case FieldKind.String: return v.S;
                case FieldKind.Bytes: return v.Bytes;
            }
            return null;
        }
    }
}
