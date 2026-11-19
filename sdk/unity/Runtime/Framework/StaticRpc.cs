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
        public MethodInfo method,
            impl;
        public RpcAttribute attr;
        public Type[] paramTypes;
        public bool wantsInfo;
        public uint hash;
    }

    public static class StaticRpcs
    {
        static Dictionary<uint, StaticRpcMethod> _byHash;
        static readonly object _lock = new object();

        public static uint Hash(string typeFullName, string method, int argCount) =>
            Fnv(typeFullName + "::" + method + "/" + argCount);

        public static uint Fnv(string text)
        {
            uint hash = 2166136261;
            foreach (char character in text)
            {
                hash ^= character;
                hash *= 16777619;
            }
            return hash;
        }

        public static StaticRpcMethod Find(uint hash)
        {
            EnsureScanned();
            return _byHash.TryGetValue(hash, out var method) ? method : null;
        }

        public static IEnumerable<StaticRpcMethod> All
        {
            get
            {
                EnsureScanned();
                return _byHash.Values;
            }
        }

        /// <summary>Scans every loaded non-system assembly once; call RescanAssemblies after loading plugins.</summary>
        public static void RescanAssemblies()
        {
            lock (_lock)
            {
                _byHash = null;
            }

            EnsureScanned();
        }

        static void EnsureScanned()
        {
            if (_byHash != null)
            {
                return;
            }

            lock (_lock)
            {
                if (_byHash != null)
                {
                    return;
                }

                var map = new Dictionary<uint, StaticRpcMethod>();
                foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
                {
                    var name = asm.GetName().Name;
                    if (
                        asm.IsDynamic
                        || name.StartsWith("System")
                        || name.StartsWith("Microsoft")
                        || name == "mscorlib"
                        || name == "netstandard"
                        || name.StartsWith("UnityEngine")
                        || name.StartsWith("UnityEditor")
                        || name.StartsWith("Unity.")
                        || name.StartsWith("Mono.")
                    )
                    {
                        continue;
                    }

                    Type[] types;
                    try
                    {
                        types = asm.GetTypes();
                    }
                    catch (ReflectionTypeLoadException error)
                    {
                        types = error.Types.Where(type => type != null).ToArray();
                    }
                    foreach (var type in types)
                    {
                        const BindingFlags Flags =
                            BindingFlags.Static
                            | BindingFlags.Public
                            | BindingFlags.NonPublic
                            | BindingFlags.DeclaredOnly;
                        foreach (var candidate in type.GetMethods(Flags))
                        {
                            var attribute = candidate.GetCustomAttribute<RpcAttribute>();
                            if (attribute == null)
                            {
                                continue;
                            }

                            var ps = candidate.GetParameters();
                            bool info =
                                ps.Length > 0 && ps[ps.Length - 1].ParameterType == typeof(RpcInfo);
                            int argc = info ? ps.Length - 1 : ps.Length;
                            var impl = type.GetMethod(
                                candidate.Name + NetworkBehaviour.ImplSuffix,
                                Flags,
                                null,
                                ps.Select(parameter => parameter.ParameterType).ToArray(),
                                null
                            );
                            var entry = new StaticRpcMethod
                            {
                                method = candidate,
                                impl = impl,
                                attr = attribute,
                                wantsInfo = info,
                                paramTypes = ps.Take(argc)
                                    .Select(parameter => parameter.ParameterType)
                                    .ToArray(),
                                hash = Hash(type.FullName, candidate.Name, argc),
                            };
                            if (
                                map.TryGetValue(entry.hash, out var other)
                                && other.method != candidate
                            )
                            {
                                throw new NetException(
                                    $"static RPC hash collision: {type.FullName}.{candidate.Name} vs {other.method.DeclaringType.FullName}.{other.method.Name}"
                                );
                            }

                            map[entry.hash] = entry;
                        }
                    }
                }
                _byHash = map;
            }
        }

        internal static void WriteArgs(BitWriter writer, StaticRpcMethod method, object[] args)
        {
            if (args.Length != method.paramTypes.Length)
            {
                throw new NetException(
                    $"{method.method.DeclaringType.Name}.{method.method.Name}: expected {method.paramTypes.Length} args, got {args.Length}"
                );
            }

            for (int i = 0; i < args.Length; i++)
            {
                NetSerializer.Write(writer, args[i], method.paramTypes[i]);
            }
        }

        internal static void Invoke(StaticRpcMethod method, BitReader reader, RpcInfo info)
        {
            var args = new object[method.paramTypes.Length + (method.wantsInfo ? 1 : 0)];
            for (int i = 0; i < method.paramTypes.Length; i++)
            {
                args[i] = NetSerializer.Read(reader, method.paramTypes[i]);
            }

            if (method.wantsInfo)
            {
                args[args.Length - 1] = info;
            }
            (method.impl ?? method.method).Invoke(null, args);
        }
    }
}
