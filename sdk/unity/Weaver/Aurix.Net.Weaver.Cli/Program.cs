// aurix-weave <assembly.dll> [--ref <dir>]... : weaves RPC methods in place (plain .NET builds, tests, CI).
using System;
using System.Collections.Generic;
using System.IO;
using Aurix.Net.Weaver;
using Mono.Cecil;

static class Program
{
    static int Main(string[] args)
    {
        if (args.Length == 0) { Console.Error.WriteLine("usage: aurix-weave <assembly.dll> [--ref <dir>]..."); return 2; }
        if (args[0] == "--dump" && args.Length >= 3)
        {
            var a = AssemblyDefinition.ReadAssembly(Path.GetFullPath(args[1]));
            foreach (var t in a.MainModule.GetTypes()) foreach (var m in t.Methods) if ((t.Name + "::" + m.Name).EndsWith(args[2]) && m.HasBody)
            { Console.WriteLine($"--- {t.FullName}::{m.Name}({string.Join(", ", System.Linq.Enumerable.Select(m.Parameters, x => x.ParameterType.Name))}) attrs={m.Attributes} customAttrs={m.CustomAttributes.Count}"); foreach (var i in m.Body.Instructions) Console.WriteLine("    " + i); }
            return 0;
        }
        var path = Path.GetFullPath(args[0]);
        var resolver = new DefaultAssemblyResolver();
        resolver.AddSearchDirectory(Path.GetDirectoryName(path));
        for (int i = 1; i + 1 < args.Length; i++) if (args[i] == "--ref") resolver.AddSearchDirectory(args[++i]);
        var pdb = Path.ChangeExtension(path, ".pdb"); bool symbols = File.Exists(pdb);
        var rp = new ReaderParameters { AssemblyResolver = resolver, ReadWrite = true, ReadSymbols = symbols, InMemory = true };
        using var asm = AssemblyDefinition.ReadAssembly(path, rp);
        var res = RpcWeaver.Process(asm.MainModule);
        foreach (var d in res.Diagnostics) Console.Error.WriteLine("weaver: " + d);
        if (!res.Changed) { Console.WriteLine($"aurix-weave: nothing to do in {Path.GetFileName(path)}"); return 0; }
        asm.Write(path, new WriterParameters { WriteSymbols = symbols });
        Console.WriteLine($"aurix-weave: {res.Methods} RPC method(s) woven in {Path.GetFileName(path)}");
        return 0;
    }
}
