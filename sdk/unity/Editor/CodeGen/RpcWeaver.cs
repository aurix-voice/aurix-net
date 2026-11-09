// IL weaver for the gameplay framework: turns a method marked [ServerRpc] / [ObserversRpc] /
// [TargetRpc] into a direct call site. For `void CmdHit(int dmg, RpcInfo info = default)` it emits
//
//   void CmdHit(int dmg, RpcInfo info) { if (__RpcSend(IDX, new object[] { dmg })) return; CmdHit__Impl(dmg, __LocalInfo()); }
//   void CmdHit__Impl(int dmg, RpcInfo info) { /* original body */ }
//
// IDX is the index the runtime assigns in NetworkBehaviour.Reflect (declaring type full name, method
// name, parameter count) — both sides must agree, so the ordering here mirrors it exactly. Runs inside
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

        public sealed class Result { public int Methods; public List<string> Diagnostics = new List<string>(); public bool Changed => Methods > 0; internal readonly Dictionary<MethodDefinition, MethodDefinition> Redirect = new Dictionary<MethodDefinition, MethodDefinition>(); }

        /// <summary>Weaves every NetworkBehaviour subclass in the module. Idempotent (marker attribute).</summary>
        public static Result Process(ModuleDefinition module)
        {
            var res = new Result();
            if (module.Types.Any(t => t.Name == MarkerType)) { res.Diagnostics.Add("already woven"); return res; }
            var behaviours = module.GetTypes().Where(t => t.IsClass && !t.IsAbstract || t.IsClass).Where(t => DerivesFrom(t, BehaviourType)).ToList();
            if (behaviours.Count == 0) return res;
            foreach (var t in behaviours)
            {
                foreach (var entry in RpcIndex(t))
                {
                    if (entry.method.Module != module) continue; // inherited from another assembly: woven there
                    if (entry.method.Body == null) { res.Diagnostics.Add($"{t.FullName}.{entry.method.Name}: no body"); continue; }
                    if (entry.method.ReturnType.MetadataType != MetadataType.Void) { res.Diagnostics.Add($"{t.FullName}.{entry.method.Name}: RPC methods must return void"); continue; }
                    if (entry.method.DeclaringType != t) continue; // base-class method: woven when its own type is processed
                    Weave(entry.method, entry.index, entry.wantsInfo, res);
                    res.Methods++;
                }
            }
            if (res.Methods > 0)
            {
                // call sites inside this module still point at the renamed original (same MethodDefinition
                // object); route them to the new public method so direct calls go through the prologue
                foreach (var t in module.GetTypes())
                    foreach (var m in t.Methods)
                    {
                        if (!m.HasBody || res.Redirect.ContainsValue(m)) continue;
                        foreach (var ins in m.Body.Instructions)
                            if (ins.Operand is MethodDefinition md && res.Redirect.TryGetValue(md, out var fresh)) ins.Operand = fresh;
                    }
                var marker = new TypeDefinition("", MarkerType, TypeAttributes.NotPublic | TypeAttributes.Sealed | TypeAttributes.Abstract | TypeAttributes.Class, module.ImportReference(typeof(object)));
                module.Types.Add(marker);
            }
            return res;
        }

        public struct Entry { public MethodDefinition method; public int index; public bool wantsInfo; }

        /// <summary>Same ordering as NetworkBehaviour.Reflect: walk the hierarchy up to NetworkBehaviour, collect
        /// attributed methods, order by (declaring type full name, name, parameter count without RpcInfo).</summary>
        public static List<Entry> RpcIndex(TypeDefinition t)
        {
            var all = new List<Entry>();
            for (var cur = t; cur != null && cur.FullName != BehaviourType; cur = Resolve(cur.BaseType))
            {
                foreach (var m in cur.Methods)
                {
                    if (m.IsStatic || m.IsConstructor || !HasRpcAttribute(m)) continue;
                    bool info = m.Parameters.Count > 0 && m.Parameters[m.Parameters.Count - 1].ParameterType.FullName == RpcInfoType;
                    all.Add(new Entry { method = m, wantsInfo = info, index = info ? m.Parameters.Count - 1 : m.Parameters.Count });
                }
            }
            var ordered = all.OrderBy(e => e.method.DeclaringType.FullName, StringComparer.Ordinal).ThenBy(e => e.method.Name, StringComparer.Ordinal).ThenBy(e => e.index).ToList();
            for (int i = 0; i < ordered.Count; i++) { var e = ordered[i]; e.index = i; ordered[i] = e; }
            return ordered;
        }

        static void Weave(MethodDefinition m, int index, bool wantsInfo, Result res)
        {
            var type = m.DeclaringType; var module = m.Module;
            if (type.Methods.Any(x => x.Name == m.Name + ImplSuffix && x.Parameters.Count == m.Parameters.Count)) return;

            // 1. the original method becomes Method__Impl (private, attributes moved off it); a fresh method
            //    with the original name, signature and attributes takes its place
            var impl = m;
            var fresh = new MethodDefinition(m.Name, m.Attributes, m.ReturnType);
            foreach (var p in m.Parameters) fresh.Parameters.Add(new ParameterDefinition(p.Name, p.Attributes, p.ParameterType) { Constant = p.HasConstant ? p.Constant : null });
            foreach (var a in m.CustomAttributes) fresh.CustomAttributes.Add(a);
            m.CustomAttributes.Clear();
            foreach (var o in m.Overrides) fresh.Overrides.Add(o);
            m.Overrides.Clear();
            m.Name = m.Name + ImplSuffix;
            m.Attributes = (m.Attributes & ~MethodAttributes.MemberAccessMask & ~MethodAttributes.Virtual & ~MethodAttributes.NewSlot & ~MethodAttributes.Abstract & ~MethodAttributes.Final) | MethodAttributes.Private | MethodAttributes.HideBySig;
            type.Methods.Add(fresh);
            res.Redirect[impl] = fresh;
            m = fresh;

            // 2. new body for Method: if (__RpcSend(index, args)) return; Method__Impl(args..., __LocalInfo());
            var body = m.Body = new MethodBody(m);
            var il = body.GetILProcessor();
            var send = module.ImportReference(FindMethod(type, "__RpcSend"));
            var localInfo = module.ImportReference(FindMethod(type, "__LocalInfo"));
            var objType = module.TypeSystem.Object;
            int n = wantsInfo ? m.Parameters.Count - 1 : m.Parameters.Count;

            il.Emit(OpCodes.Ldarg_0);
            il.Emit(OpCodes.Ldc_I4, index);
            il.Emit(OpCodes.Ldc_I4, n);
            il.Emit(OpCodes.Newarr, objType);
            for (int i = 0; i < n; i++)
            {
                il.Emit(OpCodes.Dup);
                il.Emit(OpCodes.Ldc_I4, i);
                il.Emit(OpCodes.Ldarg, m.Parameters[i]);
                var pt = m.Parameters[i].ParameterType;
                if (pt.IsValueType || pt.IsGenericParameter) il.Emit(OpCodes.Box, pt);
                il.Emit(OpCodes.Stelem_Ref);
            }
            il.Emit(OpCodes.Callvirt, send);
            var runBody = il.Create(OpCodes.Nop);
            il.Emit(OpCodes.Brfalse, runBody);
            il.Emit(OpCodes.Ret);
            il.Append(runBody);
            il.Emit(OpCodes.Ldarg_0);
            for (int i = 0; i < n; i++) il.Emit(OpCodes.Ldarg, m.Parameters[i]);
            if (wantsInfo) { il.Emit(OpCodes.Ldarg_0); il.Emit(OpCodes.Call, localInfo); }
            il.Emit(OpCodes.Call, impl);
            il.Emit(OpCodes.Ret);
            body.InitLocals = true;
            body.OptimizeMacros();
        }

        static MethodReference FindMethod(TypeDefinition t, string name)
        {
            for (var cur = t; cur != null; cur = Resolve(cur.BaseType))
            {
                var m = cur.Methods.FirstOrDefault(x => x.Name == name);
                if (m != null) return m;
            }
            throw new InvalidOperationException("NetworkBehaviour." + name + " not found — framework version mismatch");
        }

        static bool HasRpcAttribute(MethodDefinition m)
        {
            foreach (var a in m.CustomAttributes)
            {
                var at = Resolve(a.AttributeType);
                for (var cur = at; cur != null; cur = Resolve(cur.BaseType)) if (cur.FullName == RpcAttributeType) return true;
            }
            return false;
        }

        static bool DerivesFrom(TypeDefinition t, string baseName)
        {
            for (var cur = Resolve(t.BaseType); cur != null; cur = Resolve(cur.BaseType)) if (cur.FullName == baseName) return true;
            return false;
        }

        static TypeDefinition Resolve(TypeReference r)
        {
            if (r == null) return null;
            try { return r.Resolve(); } catch { return null; }
        }
    }
}
