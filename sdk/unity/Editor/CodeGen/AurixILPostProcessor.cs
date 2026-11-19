// Unity ILPostProcessor glue: runs RpcWeaver on every player/editor assembly that references
// Aurix.Net, right after the C# compiler, so [ServerRpc]/[ObserversRpc]/[TargetRpc] methods become
// direct call sites. The codegen assembly is editor-only, with no engine references and Mono.Cecil
// from Unity's own precompiled copy. Not yet run in a Unity editor;
// the weaver core is verified on plain .NET builds.
using System.IO;
using System.Linq;
using Mono.Cecil;
using Mono.Cecil.Cil;
using Unity.CompilationPipeline.Common.Diagnostics;
using Unity.CompilationPipeline.Common.ILPostProcessing;

namespace Aurix.Net.CodeGen
{
    internal sealed class AurixILPostProcessor : ILPostProcessor
    {
        const string Runtime = "Aurix.Net";

        public override ILPostProcessor GetInstance() => this;

        public override bool WillProcess(ICompiledAssembly assembly) =>
            assembly.Name == Runtime
            || assembly.References.Any(reference =>
                Path.GetFileNameWithoutExtension(reference) == Runtime
            );

        public override ILPostProcessResult Process(ICompiledAssembly assembly)
        {
            if (!WillProcess(assembly))
            {
                return null;
            }

            var diagnostics = new System.Collections.Generic.List<DiagnosticMessage>();
            var resolver = new DefaultAssemblyResolver();
            foreach (var dir in assembly.References.Select(Path.GetDirectoryName).Distinct())
            {
                resolver.AddSearchDirectory(dir);
            }

            var rp = new ReaderParameters
            {
                AssemblyResolver = resolver,
                ReadingMode = ReadingMode.Immediate,
                ReadWrite = false,
                InMemory = true,
                SymbolStream = new MemoryStream(assembly.InMemoryAssembly.PdbData),
                SymbolReaderProvider = new PortablePdbReaderProvider(),
                ReadSymbols = true,
            };
            using var asm = AssemblyDefinition.ReadAssembly(
                new MemoryStream(assembly.InMemoryAssembly.PeData),
                rp
            );
            var res = Weaver.RpcWeaver.Process(asm.MainModule);
            foreach (var diagnostic in res.Diagnostics)
            {
                diagnostics.Add(
                    new DiagnosticMessage
                    {
                        DiagnosticType = DiagnosticType.Warning,
                        MessageData = "Aurix.Net weaver: " + diagnostic,
                    }
                );
            }

            if (!res.Changed)
            {
                return new ILPostProcessResult(assembly.InMemoryAssembly, diagnostics);
            }

            var pe = new MemoryStream();
            var pdb = new MemoryStream();
            asm.Write(
                pe,
                new WriterParameters
                {
                    SymbolWriterProvider = new PortablePdbWriterProvider(),
                    SymbolStream = pdb,
                    WriteSymbols = true,
                }
            );
            return new ILPostProcessResult(
                new InMemoryAssembly(pe.ToArray(), pdb.ToArray()),
                diagnostics
            );
        }
    }
}
