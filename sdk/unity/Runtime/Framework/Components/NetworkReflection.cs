// Aurix Net Unity SDK — replicate fields/properties and call methods of any plain component by name (PurrNet-style NetworkReflection).
// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using System.Reflection;
using UnityEngine;
using Aurix.Net.Managed;

namespace Aurix.Net.Unity.Framework
{
    public enum ReflectionRpcType : byte { Server, Observers, Target }

    [Serializable]
    public struct ReflectedMethod
    {
        public string name; public ReflectionRpcType rpcType;
        public ReflectedMethod(string name, ReflectionRpcType rpcType) { this.name = name; this.rpcType = rpcType; }
    }

    /// <summary>
    /// Networks a component you cannot (or do not want to) edit: listed fields/properties of <see cref="trackedBehaviour"/>
    /// are replicated when they change (full state to late joiners), listed methods can be called over the network with
    /// <see cref="Invoke"/>. Values must be types <see cref="NetSerializer"/> understands.
    /// </summary>
    public class NetworkReflection : NetworkBehaviour
    {
        [Tooltip("The object whose members are tracked (any component or plain object).")] public object trackedBehaviour;
        public List<string> trackedFields = new List<string>();
        public List<ReflectedMethod> trackedMethods = new List<ReflectedMethod>();
        [Tooltip("Owner samples and sends (otherwise the server).")] public bool ownerAuth;
        [Tooltip("Max sends per second (0 = every tick).")] public float sendRate;

        ReflectionSync _sync;
        /// <summary>Fired on receivers after a tracked member was updated (member index).</summary>
        public event Action<int> onMemberChanged;

        protected override void OnBeforeBind()
        {
            _sync = new ReflectionSync(ownerAuth, sendRate > 0 ? 1f / sendRate : 0f);
            _sync.Configure(trackedBehaviour, trackedFields);
            _sync.onChanged += i => onMemberChanged?.Invoke(i);
        }

        public override void OnTick(uint tick) { if (isController) _sync.Sample(); }

        /// <summary>Re-read the tracked members now (e.g. after changing many at once) instead of waiting for the tick.</summary>
        public void MarkDirty() { if (isController) _sync.Sample(); }

        /// <summary>Call a tracked method over the network according to its configured RPC type (Target type: first arg is the PlayerID).</summary>
        public bool Invoke(string method, params object[] args)
        {
            var (rm, mi) = Find(method); if (mi == null) return false;
            if (rm.rpcType == ReflectionRpcType.Target)
            {
                if (args.Length == 0 || !(args[0] is PlayerID to)) { Debug.LogError($"NetworkReflection: Target method {method} needs a PlayerID first argument"); return false; }
                var rest = new object[args.Length - 1]; Array.Copy(args, 1, rest, 0, rest.Length);
                var bytes = Pack(mi, rest);
                if (isServer) { if (to == localPlayer) Dispatch(mi, bytes); else Rpc(nameof(ReflTarget), to, method, bytes); }
                else Rpc(nameof(ReflTargetViaServer), to, method, bytes);
                return true;
            }
            var payload = Pack(mi, args);
            switch (rm.rpcType)
            {
                case ReflectionRpcType.Server:
                    if (isServer) Dispatch(mi, payload); else Rpc(nameof(ReflServer), method, payload);
                    return true;
                default:
                    if (isServer) Rpc(nameof(ReflObservers), method, payload); else Rpc(nameof(ReflForward), method, payload);
                    return true;
            }
        }

        PlayerID? localPlayer => networkManager?.localPlayer;

        (ReflectedMethod, MethodInfo) Find(string name)
        {
            if (trackedBehaviour == null) return (default, null);
            foreach (var rm in trackedMethods) if (rm.name == name)
            {
                var mi = trackedBehaviour.GetType().GetMethod(name, BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                if (mi == null) Debug.LogError($"NetworkReflection: method {name} not found on {trackedBehaviour.GetType().Name}");
                return (rm, mi);
            }
            Debug.LogError($"NetworkReflection: method {name} is not in trackedMethods"); return (default, null);
        }

        static byte[] Pack(MethodInfo mi, object[] args)
        {
            var ps = mi.GetParameters(); var w = new BitWriter();
            for (int i = 0; i < ps.Length; i++) NetSerializer.Write(w, i < args.Length ? args[i] : null, ps[i].ParameterType);
            return w.Finish();
        }

        void Dispatch(MethodInfo mi, byte[] payload)
        {
            var ps = mi.GetParameters(); var r = new BitReader(payload); var args = new object[ps.Length];
            for (int i = 0; i < ps.Length; i++) args[i] = NetSerializer.Read(r, ps[i].ParameterType);
            mi.Invoke(trackedBehaviour, args);
        }

        void DispatchNamed(string name, byte[] payload) { var (_, mi) = Find(name); if (mi != null) Dispatch(mi, payload); }

        [ServerRpc(requireOwnership: false)] void ReflServer(string name, byte[] payload) => DispatchNamed(name, payload);
        [ServerRpc(requireOwnership: false)] void ReflForward(string name, byte[] payload) => Rpc(nameof(ReflObservers), name, payload);
        [ObserversRpc(runLocally: true)] void ReflObservers(string name, byte[] payload) => DispatchNamed(name, payload);
        [ServerRpc(requireOwnership: false)] void ReflTargetViaServer(PlayerID to, string name, byte[] payload) { if (to == localPlayer) DispatchNamed(name, payload); else Rpc(nameof(ReflTarget), to, name, payload); }
        [TargetRpc] void ReflTarget(PlayerID to, string name, byte[] payload) => DispatchNamed(name, payload);
    }

    /// <summary>Module behind <see cref="NetworkReflection"/>: snapshot of the tracked members, change mask deltas.</summary>
    public sealed class ReflectionSync : NetworkModule
    {
        object _target; MemberInfo[] _members = Array.Empty<MemberInfo>(); Type[] _types = Array.Empty<Type>();
        object[] _last = Array.Empty<object>(); bool[] _changed = Array.Empty<bool>();
        public event Action<int> onChanged;
        public int memberCount => _members.Length;

        public ReflectionSync(bool ownerAuth, float sendInterval) { this.ownerAuth = ownerAuth; sendIntervalInSeconds = sendInterval; }

        public void Configure(object target, List<string> names)
        {
            _target = target; var ms = new List<MemberInfo>(); var ts = new List<Type>();
            if (target != null) foreach (var n in names)
            {
                var t = target.GetType();
                var f = t.GetField(n, BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                if (f != null) { ms.Add(f); ts.Add(f.FieldType); continue; }
                var p = t.GetProperty(n, BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
                if (p != null && p.CanRead && p.CanWrite) { ms.Add(p); ts.Add(p.PropertyType); continue; }
                Debug.LogError($"NetworkReflection: {t.Name} has no field/property {n}");
            }
            _members = ms.ToArray(); _types = ts.ToArray(); _last = new object[_members.Length]; _changed = new bool[_members.Length];
            for (int i = 0; i < _members.Length; i++) _last[i] = Get(i);
        }

        object Get(int i) => _members[i] is FieldInfo f ? f.GetValue(_target) : ((PropertyInfo)_members[i]).GetValue(_target);
        void Set(int i, object v) { if (_members[i] is FieldInfo f) f.SetValue(_target, v); else ((PropertyInfo)_members[i]).SetValue(_target, v); }

        /// <summary>Controller: compare the live members with the last sent values.</summary>
        public void Sample()
        {
            for (int i = 0; i < _members.Length; i++)
            {
                var v = Get(i);
                if (!Equals(v, _last[i])) { _last[i] = v; _changed[i] = true; isDirty = true; }
            }
        }

        public override void WriteFull(BitWriter w) { for (int i = 0; i < _members.Length; i++) NetSerializer.Write(w, Get(i), _types[i]); }
        public override void ReadFull(BitReader r) { for (int i = 0; i < _members.Length; i++) { var v = NetSerializer.Read(r, _types[i]); _last[i] = v; Set(i, v); onChanged?.Invoke(i); } }
        public override void WriteDelta(BitWriter w)
        {
            for (int i = 0; i < _members.Length; i++) { w.WriteBool(_changed[i]); if (_changed[i]) { NetSerializer.Write(w, _last[i], _types[i]); _changed[i] = false; } }
        }
        public override void ReadDelta(BitReader r)
        {
            for (int i = 0; i < _members.Length; i++) if (r.ReadBool()) { var v = NetSerializer.Read(r, _types[i]); _last[i] = v; Set(i, v); onChanged?.Invoke(i); }
        }
    }
}
