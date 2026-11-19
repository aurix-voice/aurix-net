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
        if (args.Length == 0)
        {
            Console.Error.WriteLine("usage: aurix-weave <assembly.dll> [--ref <dir>]...");
            return 2;
        }
        if (args[0] == "--dump" && args.Length >= 3)
        {
            var assembly = AssemblyDefinition.ReadAssembly(Path.GetFullPath(args[1]));
            foreach (var type in assembly.MainModule.GetTypes())
            {
                foreach (var method in type.Methods)
                {
                    if ((type.Name + "::" + method.Name).EndsWith(args[2]) && method.HasBody)
                    {
                        Console.WriteLine(
                            $"--- {type.FullName}::{method.Name}({string.Join(", ", System.Linq.Enumerable.Select(method.Parameters, x => x.ParameterType.Name))}) attrs={method.Attributes} customAttrs={method.CustomAttributes.Count}"
                        );
                        foreach (var instruction in method.Body.Instructions)
                        {
                            Console.WriteLine("    " + instruction);
                        }
                    }
                }
            }

            return 0;
        }
        var path = Path.GetFullPath(args[0]);
        var resolver = new DefaultAssemblyResolver();
        resolver.AddSearchDirectory(Path.GetDirectoryName(path));
        for (int i = 1; i + 1 < args.Length; i++)
        {
            if (args[i] == "--ref")
            {
                resolver.AddSearchDirectory(args[++i]);
            }
        }

        var pdb = Path.ChangeExtension(path, ".pdb");
        bool symbols = File.Exists(pdb);
        var rp = new ReaderParameters
        {
            AssemblyResolver = resolver,
            ReadWrite = true,
            ReadSymbols = symbols,
            InMemory = true,
        };
        using var asm = AssemblyDefinition.ReadAssembly(path, rp);
        var res = RpcWeaver.Process(asm.MainModule);
        foreach (var diagnostic in res.Diagnostics)
        {
            Console.Error.WriteLine("weaver: " + diagnostic);
        }

        if (!res.Changed)
        {
            Console.WriteLine($"aurix-weave: nothing to do in {Path.GetFileName(path)}");
            return 0;
        }
        asm.Write(path, new WriterParameters { WriteSymbols = symbols });
        Console.WriteLine(
            $"aurix-weave: {res.Methods} RPC method(s) woven in {Path.GetFileName(path)}"
        );
        return 0;
    }
}
