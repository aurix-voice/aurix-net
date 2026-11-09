// Static RPCs: [ServerRpc] / [ObserversRpc] / [TargetRpc] on static methods of any class, no NetworkIdentity
// involved. Identified on the wire by a 32-bit FNV-1a of "Type.FullName::Method/argCount" so the weaver
// (one assembly at a time) and the runtime (all loaded assemblies) agree without a shared index.
using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    public sealed class StaticRpcMethod
    {
        public MethodInfo method, impl; public RpcAttribute attr; public Type[] paramTypes; public bool wantsInfo; public uint hash;
    }

    public static class StaticRpcs
    {
        static Dictionary<uint, StaticRpcMethod> _byHash;
        static readonly object _lock = new object();

        public static uint Hash(string typeFullName, string method, int argCount) => Fnv(typeFullName + "::" + method + "/" + argCount);

        public static uint Fnv(string s)
        {
            uint h = 2166136261;
            foreach (char c in s) { h ^= c; h *= 16777619; }
            return h;
        }

        public static StaticRpcMethod Find(uint hash) { EnsureScanned(); return _byHash.TryGetValue(hash, out var m) ? m : null; }
        public static IEnumerable<StaticRpcMethod> All { get { EnsureScanned(); return _byHash.Values; } }

        /// <summary>Scans every loaded non-system assembly once; call RescanAssemblies after loading plugins.</summary>
        public static void RescanAssemblies() { lock (_lock) _byHash = null; EnsureScanned(); }

        static void EnsureScanned()
        {
            if (_byHash != null) return;
            lock (_lock)
            {
                if (_byHash != null) return;
                var map = new Dictionary<uint, StaticRpcMethod>();
                foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
                {
                    var n = asm.GetName().Name;
                    if (asm.IsDynamic || n.StartsWith("System") || n.StartsWith("Microsoft") || n == "mscorlib" || n == "netstandard" || n.StartsWith("UnityEngine") || n.StartsWith("UnityEditor") || n.StartsWith("Unity.") || n.StartsWith("Mono.")) continue;
                    Type[] types; try { types = asm.GetTypes(); } catch (ReflectionTypeLoadException e) { types = e.Types.Where(t => t != null).ToArray(); }
                    foreach (var t in types)
                    {
                        const BindingFlags F = BindingFlags.Static | BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.DeclaredOnly;
                        foreach (var m in t.GetMethods(F))
                        {
                            var a = m.GetCustomAttribute<RpcAttribute>(); if (a == null) continue;
                            var ps = m.GetParameters();
                            bool info = ps.Length > 0 && ps[ps.Length - 1].ParameterType == typeof(RpcInfo);
                            int argc = info ? ps.Length - 1 : ps.Length;
                            var impl = t.GetMethod(m.Name + NetworkBehaviour.ImplSuffix, F, null, ps.Select(p => p.ParameterType).ToArray(), null);
                            var e = new StaticRpcMethod { method = m, impl = impl, attr = a, wantsInfo = info, paramTypes = ps.Take(argc).Select(p => p.ParameterType).ToArray(), hash = Hash(t.FullName, m.Name, argc) };
                            if (map.TryGetValue(e.hash, out var other) && other.method != m) throw new NetException($"static RPC hash collision: {t.FullName}.{m.Name} vs {other.method.DeclaringType.FullName}.{other.method.Name}");
                            map[e.hash] = e;
                        }
                    }
                }
                _byHash = map;
            }
        }

        internal static void WriteArgs(BitWriter w, StaticRpcMethod r, object[] args)
        {
            if (args.Length != r.paramTypes.Length) throw new NetException($"{r.method.DeclaringType.Name}.{r.method.Name}: expected {r.paramTypes.Length} args, got {args.Length}");
            for (int i = 0; i < args.Length; i++) NetSerializer.Write(w, args[i], r.paramTypes[i]);
        }

        internal static void Invoke(StaticRpcMethod r, BitReader reader, RpcInfo info)
        {
            var args = new object[r.paramTypes.Length + (r.wantsInfo ? 1 : 0)];
            for (int i = 0; i < r.paramTypes.Length; i++) args[i] = NetSerializer.Read(reader, r.paramTypes[i]);
            if (r.wantsInfo) args[args.Length - 1] = info;
            (r.impl ?? r.method).Invoke(null, args);
        }
    }
}
