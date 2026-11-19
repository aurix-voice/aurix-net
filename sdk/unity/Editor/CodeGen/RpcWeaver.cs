// IL weaver for the gameplay framework: turns a method marked [ServerRpc] / [ObserversRpc] /
// [TargetRpc] into a direct call site. For `void CmdHit(int dmg, RpcInfo info = default)` it emits
//
//   void CmdHit(int dmg, RpcInfo info) { if (__RpcSend(IDX, new object[] { dmg })) return; CmdHit__Impl(dmg, __LocalInfo()); }
//   void CmdHit__Impl(int dmg, RpcInfo info) { /* original body */ }
//
// IDX is the index the runtime assigns in NetworkBehaviour.Reflect (declaring type full name, method
// name, parameter count): both sides must agree, so the ordering here mirrors it exactly. Runs inside
// Unity through the ILPostProcessor in Editor/CodeGen, or from the CLI for plain .NET builds.
using System;
using System.Collections.Generic;
using System.Linq;
using Mono.Cecil;
using Mono.Cecil.Cil;
using Mono.Cecil.Rocks;

namespace Aurix.Net.Weaver
{
    public static class RpcWeaver
    {
        public const string BehaviourType = "Aurix.Net.Unity.Framework.NetworkBehaviour";
        public const string RpcAttributeType = "Aurix.Net.Unity.Framework.RpcAttribute";
        public const string RpcInfoType = "Aurix.Net.Unity.Framework.RpcInfo";
        public const string ImplSuffix = "__Impl";
        public const string MarkerType = "AurixNetWoven";
        public const string ManagerType = "Aurix.Net.Unity.Framework.NetworkManager";

        public sealed class Result
        {
            public int Methods;
            public List<string> Diagnostics = new List<string>();
            public bool Changed => Methods > 0;
            internal readonly Dictionary<MethodDefinition, MethodDefinition> Redirect =
                new Dictionary<MethodDefinition, MethodDefinition>();
        }

        /// <summary>Weaves every NetworkBehaviour subclass in the module. Idempotent (marker attribute).</summary>
        public static Result Process(ModuleDefinition module)
        {
            var res = new Result();
            if (module.Types.Any(type => type.Name == MarkerType))
            {
                res.Diagnostics.Add("already woven");
                return res;
            }
            var behaviours = module
                .GetTypes()
                .Where(type => type.IsClass && !type.IsAbstract || type.IsClass)
                .Where(type => DerivesFrom(type, BehaviourType))
                .ToList();
            var statics = module
                .GetTypes()
                .SelectMany(type => type.Methods)
                .Where(method =>
                    method.IsStatic
                    && !method.IsConstructor
                    && HasRpcAttribute(method)
                    && !method.Name.EndsWith(ImplSuffix)
                )
                .ToList();
            foreach (var method in statics)
            {
                if (method.Body == null)
                {
                    res.Diagnostics.Add($"{method.DeclaringType.FullName}.{method.Name}: no body");
                    continue;
                }
                if (method.ReturnType.MetadataType != MetadataType.Void)
                {
                    res.Diagnostics.Add(
                        $"{method.DeclaringType.FullName}.{method.Name}: RPC methods must return void"
                    );
                    continue;
                }
                bool info =
                    method.Parameters.Count > 0
                    && method.Parameters[method.Parameters.Count - 1].ParameterType.FullName
                        == RpcInfoType;
                int argc = info ? method.Parameters.Count - 1 : method.Parameters.Count;
                WeaveStatic(
                    method,
                    StaticHash(method.DeclaringType.FullName, method.Name, argc),
                    info,
                    res
                );
                res.Methods++;
            }
            if (behaviours.Count == 0 && res.Methods == 0)
            {
                return res;
            }

            foreach (var type in behaviours)
            {
                foreach (var entry in RpcIndex(type))
                {
                    if (entry.method.Module != module)
                    {
                        continue; // inherited from another assembly: woven there
                    }

                    if (entry.method.Body == null)
                    {
                        res.Diagnostics.Add($"{type.FullName}.{entry.method.Name}: no body");
                        continue;
                    }
                    if (entry.method.ReturnType.MetadataType != MetadataType.Void)
                    {
                        res.Diagnostics.Add(
                            $"{type.FullName}.{entry.method.Name}: RPC methods must return void"
                        );
                        continue;
                    }
                    if (entry.method.DeclaringType != type)
                    {
                        continue; // base-class method: woven when its own type is processed
                    }

                    Weave(entry.method, entry.index, entry.wantsInfo, res);
                    res.Methods++;
                }
            }
            if (res.Methods > 0)
            {
                // call sites inside this module still point at the renamed original (same MethodDefinition
                // object); route them to the new public method so direct calls go through the prologue
                foreach (var type in module.GetTypes())
                {
                    foreach (var method in type.Methods)
                    {
                        if (!method.HasBody || res.Redirect.ContainsValue(method))
                        {
                            continue;
                        }

                        foreach (var ins in method.Body.Instructions)
                        {
                            if (
                                ins.Operand is MethodDefinition md
                                && res.Redirect.TryGetValue(md, out var fresh)
                            )
                            {
                                ins.Operand = fresh;
                            }
                        }
                    }
                }

                var marker = new TypeDefinition(
                    "",
                    MarkerType,
                    TypeAttributes.NotPublic
                        | TypeAttributes.Sealed
                        | TypeAttributes.Abstract
                        | TypeAttributes.Class,
                    module.ImportReference(typeof(object))
                );
                module.Types.Add(marker);
            }
            return res;
        }

        public struct Entry
        {
            public MethodDefinition method;
            public int index;
            public bool wantsInfo;
        }

        /// <summary>Same ordering as NetworkBehaviour.Reflect: walk the hierarchy up to NetworkBehaviour, collect
        /// attributed methods, order by (declaring type full name, name, parameter count without RpcInfo).</summary>
        public static List<Entry> RpcIndex(TypeDefinition type)
        {
            var all = new List<Entry>();
            for (
                var cur = type;
                cur != null && cur.FullName != BehaviourType;
                cur = Resolve(cur.BaseType)
            )
            {
                foreach (var candidate in cur.Methods)
                {
                    if (
                        candidate.IsStatic
                        || candidate.IsConstructor
                        || !HasRpcAttribute(candidate)
                    )
                    {
                        continue;
                    }

                    bool info =
                        candidate.Parameters.Count > 0
                        && candidate
                            .Parameters[candidate.Parameters.Count - 1]
                            .ParameterType
                            .FullName == RpcInfoType;
                    all.Add(
                        new Entry
                        {
                            method = candidate,
                            wantsInfo = info,
                            index = info
                                ? candidate.Parameters.Count - 1
                                : candidate.Parameters.Count,
                        }
                    );
                }
            }
            var ordered = all.OrderBy(
                    entry => entry.method.DeclaringType.FullName,
                    StringComparer.Ordinal
                )
                .ThenBy(entry => entry.method.Name, StringComparer.Ordinal)
                .ThenBy(entry => entry.index)
                .ToList();
            for (int i = 0; i < ordered.Count; i++)
            {
                var entry = ordered[i];
                entry.index = i;
                ordered[i] = entry;
            }
            return ordered;
        }

        /// <summary>
        /// Generic RPC methods keep their body; a prologue is inserted instead:
        /// <c>if (__RpcSendGeneric(index, new[] { typeof(T)... }, args)) return; info = __LocalInfo();</c>.
        /// On receive the runtime invokes the closed method with a re-entrancy guard so the prologue falls through.
        /// </summary>
        static void WeaveGenericInPlace(MethodDefinition method, int index, bool wantsInfo)
        {
            var type = method.DeclaringType;
            var module = method.Module;
            var body = method.Body;
            var il = body.GetILProcessor();
            var first = body.Instructions[0];
            var send = module.ImportReference(FindMethod(type, "__RpcSendGeneric"));
            var localInfo = module.ImportReference(FindMethod(type, "__LocalInfoOr"));
            var getType = module.ImportReference(typeof(Type).GetMethod("GetTypeFromHandle"));
            var objType = module.TypeSystem.Object;
            var typeType = module.ImportReference(typeof(Type));
            int count = wantsInfo ? method.Parameters.Count - 1 : method.Parameters.Count;
            var ins = new List<Instruction>();
            ins.Add(il.Create(OpCodes.Ldarg_0));
            ins.Add(il.Create(OpCodes.Ldc_I4, index));
            ins.Add(il.Create(OpCodes.Ldc_I4, method.GenericParameters.Count));
            ins.Add(il.Create(OpCodes.Newarr, typeType));
            for (int i = 0; i < method.GenericParameters.Count; i++)
            {
                ins.Add(il.Create(OpCodes.Dup));
                ins.Add(il.Create(OpCodes.Ldc_I4, i));
                ins.Add(il.Create(OpCodes.Ldtoken, method.GenericParameters[i]));
                ins.Add(il.Create(OpCodes.Call, getType));
                ins.Add(il.Create(OpCodes.Stelem_Ref));
            }
            ins.Add(il.Create(OpCodes.Ldc_I4, count));
            ins.Add(il.Create(OpCodes.Newarr, objType));
            for (int i = 0; i < count; i++)
            {
                ins.Add(il.Create(OpCodes.Dup));
                ins.Add(il.Create(OpCodes.Ldc_I4, i));
                ins.Add(il.Create(OpCodes.Ldarg, method.Parameters[i]));
                var pt = method.Parameters[i].ParameterType;
                if (pt.IsValueType || pt.IsGenericParameter)
                {
                    ins.Add(il.Create(OpCodes.Box, pt));
                }

                ins.Add(il.Create(OpCodes.Stelem_Ref));
            }
            ins.Add(il.Create(OpCodes.Callvirt, send));
            var br = il.Create(OpCodes.Brfalse, first);
            ins.Add(br);
            ins.Add(il.Create(OpCodes.Ret));
            if (wantsInfo)
            {
                // fall-through path: info = __LocalInfo(); then the original body
                var infoParam = method.Parameters[method.Parameters.Count - 1];
                var ld = il.Create(OpCodes.Ldarg_0);
                ins.Add(ld);
                ins.Add(il.Create(OpCodes.Ldarg, infoParam));
                ins.Add(il.Create(OpCodes.Call, localInfo));
                ins.Add(il.Create(OpCodes.Starg, infoParam));
                br.Operand = ld;
            }
            foreach (var instruction in ins)
            {
                il.InsertBefore(first, instruction);
            }

            body.InitLocals = true;
            body.OptimizeMacros();
        }

        static void Weave(MethodDefinition method, int index, bool wantsInfo, Result res)
        {
            var type = method.DeclaringType;
            var module = method.Module;
            if (method.HasGenericParameters)
            {
                WeaveGenericInPlace(method, index, wantsInfo);
                return;
            }
            if (
                type.Methods.Any(x =>
                    x.Name == method.Name + ImplSuffix
                    && x.Parameters.Count == method.Parameters.Count
                )
            )
            {
                return;
            }

            // 1. the original method becomes Method__Impl (private, attributes moved off it); a fresh method
            //    with the original name, signature and attributes takes its place
            var impl = method;
            var fresh = new MethodDefinition(method.Name, method.Attributes, method.ReturnType);
            foreach (var parameter in method.Parameters)
            {
                fresh.Parameters.Add(
                    new ParameterDefinition(
                        parameter.Name,
                        parameter.Attributes,
                        parameter.ParameterType
                    )
                    {
                        Constant = parameter.HasConstant ? parameter.Constant : null,
                    }
                );
            }

            foreach (var attribute in method.CustomAttributes)
            {
                fresh.CustomAttributes.Add(attribute);
            }

            method.CustomAttributes.Clear();
            foreach (var overridden in method.Overrides)
            {
                fresh.Overrides.Add(overridden);
            }

            method.Overrides.Clear();
            method.Name = method.Name + ImplSuffix;
            method.Attributes =
                (
                    method.Attributes
                    & ~MethodAttributes.MemberAccessMask
                    & ~MethodAttributes.Virtual
                    & ~MethodAttributes.NewSlot
                    & ~MethodAttributes.Abstract
                    & ~MethodAttributes.Final
                )
                | MethodAttributes.Private
                | MethodAttributes.HideBySig;
            type.Methods.Add(fresh);
            res.Redirect[impl] = fresh;
            method = fresh;

            // 2. new body for Method: if (__RpcSend(index, args)) return; Method__Impl(args..., __LocalInfo());
            var body = method.Body = new MethodBody(method);
            var il = body.GetILProcessor();
            var send = module.ImportReference(FindMethod(type, "__RpcSend"));
            var localInfo = module.ImportReference(FindMethod(type, "__LocalInfo"));
            var objType = module.TypeSystem.Object;
            int count = wantsInfo ? method.Parameters.Count - 1 : method.Parameters.Count;

            il.Emit(OpCodes.Ldarg_0);
            il.Emit(OpCodes.Ldc_I4, index);
            il.Emit(OpCodes.Ldc_I4, count);
            il.Emit(OpCodes.Newarr, objType);
            for (int i = 0; i < count; i++)
            {
                il.Emit(OpCodes.Dup);
                il.Emit(OpCodes.Ldc_I4, i);
                il.Emit(OpCodes.Ldarg, method.Parameters[i]);
                var pt = method.Parameters[i].ParameterType;
                if (pt.IsValueType || pt.IsGenericParameter)
                {
                    il.Emit(OpCodes.Box, pt);
                }

                il.Emit(OpCodes.Stelem_Ref);
            }
            il.Emit(OpCodes.Callvirt, send);
            var runBody = il.Create(OpCodes.Nop);
            il.Emit(OpCodes.Brfalse, runBody);
            il.Emit(OpCodes.Ret);
            il.Append(runBody);
            il.Emit(OpCodes.Ldarg_0);
            for (int i = 0; i < count; i++)
            {
                il.Emit(OpCodes.Ldarg, method.Parameters[i]);
            }

            if (wantsInfo)
            {
                il.Emit(OpCodes.Ldarg_0);
                il.Emit(OpCodes.Call, localInfo);
            }
            il.Emit(OpCodes.Call, impl);
            il.Emit(OpCodes.Ret);
            body.InitLocals = true;
            body.OptimizeMacros();
        }

        /// <summary>Same as StaticRpcs.Hash: FNV-1a over "Type.FullName::Method/argCount".</summary>
        public static uint StaticHash(string typeFullName, string method, int argCount)
        {
            uint hash = 2166136261;
            foreach (char character in typeFullName + "::" + method + "/" + argCount)
            {
                hash ^= character;
                hash *= 16777619;
            }
            return hash;
        }

        static void WeaveStatic(MethodDefinition method, uint hash, bool wantsInfo, Result res)
        {
            var type = method.DeclaringType;
            var module = method.Module;
            if (
                type.Methods.Any(x =>
                    x.Name == method.Name + ImplSuffix
                    && x.Parameters.Count == method.Parameters.Count
                )
            )
            {
                return;
            }

            var manager = FindManager(module);
            if (manager == null)
            {
                res.Diagnostics.Add(
                    $"{type.FullName}.{method.Name}: NetworkManager type not found"
                );
                return;
            }

            var impl = method;
            var fresh = new MethodDefinition(method.Name, method.Attributes, method.ReturnType);
            foreach (var parameter in method.Parameters)
            {
                fresh.Parameters.Add(
                    new ParameterDefinition(
                        parameter.Name,
                        parameter.Attributes,
                        parameter.ParameterType
                    )
                    {
                        Constant = parameter.HasConstant ? parameter.Constant : null,
                    }
                );
            }

            foreach (var attribute in method.CustomAttributes)
            {
                fresh.CustomAttributes.Add(attribute);
            }

            method.CustomAttributes.Clear();
            method.Name = method.Name + ImplSuffix;
            method.Attributes =
                (method.Attributes & ~MethodAttributes.MemberAccessMask) | MethodAttributes.Private;
            type.Methods.Add(fresh);
            res.Redirect[impl] = fresh;
            method = fresh;

            var body = method.Body = new MethodBody(method);
            var il = body.GetILProcessor();
            var send = module.ImportReference(
                manager.Methods.First(x => x.Name == "__StaticRpcSend")
            );
            var localInfo = module.ImportReference(
                manager.Methods.First(x => x.Name == "__StaticLocalInfo")
            );
            var objType = module.TypeSystem.Object;
            int count = wantsInfo ? method.Parameters.Count - 1 : method.Parameters.Count;

            il.Emit(OpCodes.Ldc_I4, unchecked((int)hash));
            il.Emit(OpCodes.Ldc_I4, count);
            il.Emit(OpCodes.Newarr, objType);
            for (int i = 0; i < count; i++)
            {
                il.Emit(OpCodes.Dup);
                il.Emit(OpCodes.Ldc_I4, i);
                il.Emit(OpCodes.Ldarg, method.Parameters[i]);
                var pt = method.Parameters[i].ParameterType;
                if (pt.IsValueType || pt.IsGenericParameter)
                {
                    il.Emit(OpCodes.Box, pt);
                }

                il.Emit(OpCodes.Stelem_Ref);
            }
            il.Emit(OpCodes.Call, send);
            var runBody = il.Create(OpCodes.Nop);
            il.Emit(OpCodes.Brfalse, runBody);
            il.Emit(OpCodes.Ret);
            il.Append(runBody);
            for (int i = 0; i < count; i++)
            {
                il.Emit(OpCodes.Ldarg, method.Parameters[i]);
            }

            if (wantsInfo)
            {
                il.Emit(OpCodes.Call, localInfo);
            }

            il.Emit(OpCodes.Call, impl);
            il.Emit(OpCodes.Ret);
            body.InitLocals = true;
            body.OptimizeMacros();
        }

        static TypeDefinition FindManager(ModuleDefinition module)
        {
            var own = module.GetType(ManagerType);
            if (own != null)
            {
                return own;
            }

            foreach (var ar in module.AssemblyReferences)
            {
                try
                {
                    var type = module.AssemblyResolver.Resolve(ar).MainModule.GetType(ManagerType);
                    if (type != null)
                    {
                        return type;
                    }
                }
                catch { }
            }
            return null;
        }

        static MethodReference FindMethod(TypeDefinition type, string name)
        {
            for (var cur = type; cur != null; cur = Resolve(cur.BaseType))
            {
                var method = cur.Methods.FirstOrDefault(x => x.Name == name);
                if (method != null)
                {
                    return method;
                }
            }
            throw new InvalidOperationException(
                "NetworkBehaviour." + name + " not found — framework version mismatch"
            );
        }

        static bool HasRpcAttribute(MethodDefinition method)
        {
            foreach (var attribute in method.CustomAttributes)
            {
                var at = Resolve(attribute.AttributeType);
                for (var cur = at; cur != null; cur = Resolve(cur.BaseType))
                {
                    if (cur.FullName == RpcAttributeType)
                    {
                        return true;
                    }
                }
            }
            return false;
        }

        static bool DerivesFrom(TypeDefinition type, string baseName)
        {
            for (var cur = Resolve(type.BaseType); cur != null; cur = Resolve(cur.BaseType))
            {
                if (cur.FullName == baseName)
                {
                    return true;
                }
            }

            return false;
        }

        static TypeDefinition Resolve(TypeReference reference)
        {
            if (reference == null)
            {
                return null;
            }

            try
            {
                return reference.Resolve();
            }
            catch
            {
                return null;
            }
        }
    }
}
