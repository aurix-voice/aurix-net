// Game logic on a networked object. Declare state as SyncVar<T>/SyncList<T>/SyncDictionary<K,V>
// fields and remote calls as methods marked [ServerRpc], [ObserversRpc] or [TargetRpc], invoked
// through Rpc(nameof(Method), args...) / RpcTo(player, nameof(Method), args...). Modules and RPC
// methods are bound by reflection in a deterministic order, so both peers must run the same build.
using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    public enum RpcKind : byte { Server, Observers, Target }

    public abstract class RpcAttribute : Attribute
    {
        public abstract RpcKind kind { get; }
        public bool runLocally;
        public bool reliable = true;
    }

    /// <summary>Client → server. By default only the owner may call it.</summary>
    [AttributeUsage(AttributeTargets.Method)]
    public sealed class ServerRpcAttribute : RpcAttribute
    {
        public override RpcKind kind => RpcKind.Server;
        public bool requireOwnership = true;
        public ServerRpcAttribute(bool requireOwnership = true, bool runLocally = false, bool reliable = true) { this.requireOwnership = requireOwnership; this.runLocally = runLocally; this.reliable = reliable; }
    }

    /// <summary>Server → every observer (late joiners get the last call when bufferLast is set).</summary>
    [AttributeUsage(AttributeTargets.Method)]
    public sealed class ObserversRpcAttribute : RpcAttribute
    {
        public override RpcKind kind => RpcKind.Observers;
        public bool bufferLast, excludeOwner, excludeSender;
        public ObserversRpcAttribute(bool runLocally = true, bool bufferLast = false, bool excludeOwner = false, bool reliable = true) { this.runLocally = runLocally; this.bufferLast = bufferLast; this.excludeOwner = excludeOwner; this.reliable = reliable; }
    }

    /// <summary>Server → one player (RpcTo).</summary>
    [AttributeUsage(AttributeTargets.Method)]
    public sealed class TargetRpcAttribute : RpcAttribute
    {
        public override RpcKind kind => RpcKind.Target;
        public bool bufferLast;
        public TargetRpcAttribute(bool runLocally = true, bool bufferLast = false, bool reliable = true) { this.runLocally = runLocally; this.bufferLast = bufferLast; this.reliable = reliable; }
    }

    /// <summary>Optional last parameter of an RPC method: who sent it.</summary>
    public readonly struct RpcInfo
    {
        public readonly PlayerID sender; public readonly bool asServer;
        public RpcInfo(PlayerID sender, bool asServer) { this.sender = sender; this.asServer = asServer; }
    }

    public sealed class RpcMethod
    {
        public MethodInfo method; public RpcAttribute attr; public Type[] paramTypes; public bool wantsInfo; public byte index;
        /// <summary>Woven assemblies: the original body, renamed to Method__Impl; invoked instead of <see cref="method"/> on receive.</summary>
        public MethodInfo impl;
    }

    public abstract class NetworkBehaviour : MonoBehaviour
    {
        public NetworkIdentity identity { get; private set; }
        public int componentIndex { get; private set; }
        public NetworkManager networkManager => identity?.networkManager;
        public bool isSpawned => identity != null && identity.isSpawned;
        public bool isServer => identity != null && identity.isServer;
        public bool isClient => identity != null && identity.isClient;
        public bool isHost => identity != null && identity.isHost;
        public bool isOwner => identity != null && identity.isOwner;
        public bool isController => identity != null && identity.isController;
        public PlayerID? owner => identity?.owner;
        public PlayerID? localPlayer => identity?.localPlayer;
        public uint objectId => identity?.objectId ?? 0;

        internal NetworkModule[] modules = Array.Empty<NetworkModule>();
        internal RpcMethod[] rpcs = Array.Empty<RpcMethod>();
        Dictionary<string, RpcMethod> _rpcByName;

        static readonly Dictionary<Type, (FieldInfo[] fields, RpcMethod[] rpcs)> _cache = new Dictionary<Type, (FieldInfo[], RpcMethod[])>();

        internal void Bind(NetworkIdentity id, int index)
        {
            identity = id; componentIndex = index;
            OnBeforeBind();
            var (fields, rpcList) = Reflect(GetType());
            modules = new NetworkModule[fields.Length];
            for (int i = 0; i < fields.Length; i++)
            {
                var m = (NetworkModule)fields[i].GetValue(this);
                if (m == null) { m = (NetworkModule)Activator.CreateInstance(fields[i].FieldType); fields[i].SetValue(this, m); }
                m.Bind(this, i); modules[i] = m;
            }
            rpcs = rpcList; _rpcByName = rpcs.ToDictionary(r => r.method.Name);
        }

        static (FieldInfo[], RpcMethod[]) Reflect(Type t)
        {
            if (_cache.TryGetValue(t, out var c)) return c;
            var fields = new List<FieldInfo>();
            var methods = new List<RpcMethod>();
            for (var cur = t; cur != null && cur != typeof(NetworkBehaviour); cur = cur.BaseType)
            {
                const BindingFlags F = BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.DeclaredOnly;
                fields.InsertRange(0, cur.GetFields(F).Where(f => typeof(NetworkModule).IsAssignableFrom(f.FieldType)).OrderBy(f => f.MetadataToken));
                foreach (var m in cur.GetMethods(F))
                {
                    var a = m.GetCustomAttribute<RpcAttribute>(); if (a == null) continue;
                    var ps = m.GetParameters();
                    bool info = ps.Length > 0 && ps[ps.Length - 1].ParameterType == typeof(RpcInfo);
                    var impl = cur.GetMethod(m.Name + ImplSuffix, F, null, ps.Select(p => p.ParameterType).ToArray(), null);
                    methods.Add(new RpcMethod { method = m, impl = impl, attr = a, wantsInfo = info, paramTypes = ps.Take(info ? ps.Length - 1 : ps.Length).Select(p => p.ParameterType).ToArray() });
                }
            }
            var ordered = methods.OrderBy(r => r.method.DeclaringType.FullName).ThenBy(r => r.method.Name).ThenBy(r => r.paramTypes.Length).ToArray();
            if (ordered.Length > 255) throw new NetException(t.Name + ": more than 255 RPC methods");
            for (int i = 0; i < ordered.Length; i++) ordered[i].index = (byte)i;
            var res = (fields.ToArray(), ordered);
            _cache[t] = res; return res;
        }

        internal void InternalSpawned(bool asServer) { foreach (var m in modules) m.OnSpawned(asServer); OnSpawned(asServer); }
        internal void InternalDespawned() { foreach (var m in modules) m.OnDespawned(); OnDespawned(); }
        internal void InternalOwnerChanged(PlayerID? o, PlayerID? n) { foreach (var m in modules) m.OnOwnerChanged(o, n); OnOwnerChanged(o, n); }

        /// <summary>Called right before modules are bound: create module instances that depend on inspector fields here.</summary>
        protected virtual void OnBeforeBind() { }
        /// <summary>Called once the object is live on this peer (state applied, owner known).</summary>
        protected virtual void OnSpawned(bool asServer) { }
        protected virtual void OnDespawned() { }
        protected virtual void OnOwnerChanged(PlayerID? oldOwner, PlayerID? newOwner) { }
        /// <summary>Called every network tick on the server and on clients.</summary>
        public virtual void OnTick(uint tick) { }

        /// <summary>Suffix the IL weaver gives the original body of an RPC method.</summary>
        public const string ImplSuffix = "__Impl";

        /// <summary>Call a [ServerRpc] or [ObserversRpc] method by name (reflection mode; woven assemblies call the method directly).</summary>
        protected void Rpc(string method, params object[] args) { var r = Find(method); if (networkManager != null && !networkManager.SendRpc(this, r, TargetOf(r, args), args)) InvokeRpcLocal(r, args, LocalInfo()); }
        /// <summary>Call a [TargetRpc] method on one player by name.</summary>
        protected void RpcTo(PlayerID target, string method, params object[] args) { var r = Find(method); if (networkManager != null && !networkManager.SendRpc(this, r, target, args)) InvokeRpcLocal(r, args, LocalInfo()); }

        /// <summary>Woven RPC prologue: routes the call; returns true when the body must not run here.</summary>
        protected bool __RpcSend(int index, object[] args)
        {
            if (index >= rpcs.Length || networkManager == null) return false;
            var r = rpcs[index];
            return networkManager.SendRpc(this, r, TargetOf(r, args), args);
        }

        /// <summary>Woven RPC prologue: the RpcInfo for a locally executed call.</summary>
        protected RpcInfo __LocalInfo() => LocalInfo();

        RpcInfo LocalInfo() => new RpcInfo(localPlayer ?? default, isServer);

        /// <summary>[TargetRpc] methods take the target player as their first parameter.</summary>
        static PlayerID? TargetOf(RpcMethod r, object[] args) => r.attr.kind == RpcKind.Target && args.Length > 0 && args[0] is PlayerID p ? p : (PlayerID?)null;

        RpcMethod Find(string name)
        {
            if (_rpcByName != null && _rpcByName.TryGetValue(name, out var r)) return r;
            throw new NetException($"{GetType().Name}: no RPC method '{name}' (mark it [ServerRpc]/[ObserversRpc]/[TargetRpc])");
        }

        internal void WriteArgs(BitWriter w, RpcMethod r, object[] args)
        {
            if (args.Length != r.paramTypes.Length) throw new NetException($"{GetType().Name}.{r.method.Name}: expected {r.paramTypes.Length} args, got {args.Length}");
            for (int i = 0; i < args.Length; i++) NetSerializer.Write(w, args[i], r.paramTypes[i]);
        }

        internal void InvokeRpc(RpcMethod r, BitReader reader, RpcInfo info)
        {
            var args = new object[r.paramTypes.Length + (r.wantsInfo ? 1 : 0)];
            for (int i = 0; i < r.paramTypes.Length; i++) args[i] = NetSerializer.Read(reader, r.paramTypes[i]);
            if (r.wantsInfo) args[args.Length - 1] = info;
            (r.impl ?? r.method).Invoke(this, args);
        }

        internal void InvokeRpcLocal(RpcMethod r, object[] args, RpcInfo info)
        {
            var target = r.impl ?? r.method;
            if (!r.wantsInfo) { target.Invoke(this, args); return; }
            var a = new object[args.Length + 1]; Array.Copy(args, a, args.Length); a[args.Length] = info;
            target.Invoke(this, a);
        }
    }
}
