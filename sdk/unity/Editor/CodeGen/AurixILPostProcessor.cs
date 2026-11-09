// Unity ILPostProcessor glue: runs RpcWeaver on every player/editor assembly that references
// Aurix.Net, right after the C# compiler, so [ServerRpc]/[ObserversRpc]/[TargetRpc] methods become
// direct call sites. Mirrors the structure of Netcode for GameObjects' codegen assembly: editor-only,
// no engine references, Mono.Cecil from Unity's own precompiled copy. NOT yet verified in a Unity
// editor (no licence on the build machines); the weaver core is verified on plain .NET builds.
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

        public override bool WillProcess(ICompiledAssembly a) =>
            a.Name == Runtime || a.References.Any(r => Path.GetFileNameWithoutExtension(r) == Runtime);

        public override ILPostProcessResult Process(ICompiledAssembly a)
        {
            if (!WillProcess(a)) return null;
            var diagnostics = new System.Collections.Generic.List<DiagnosticMessage>();
            var resolver = new DefaultAssemblyResolver();
            foreach (var dir in a.References.Select(Path.GetDirectoryName).Distinct()) resolver.AddSearchDirectory(dir);
            var rp = new ReaderParameters
            {
                AssemblyResolver = resolver, ReadingMode = ReadingMode.Immediate, ReadWrite = false, InMemory = true,
                SymbolStream = new MemoryStream(a.InMemoryAssembly.PdbData), SymbolReaderProvider = new PortablePdbReaderProvider(), ReadSymbols = true
            };
            using var asm = AssemblyDefinition.ReadAssembly(new MemoryStream(a.InMemoryAssembly.PeData), rp);
            var res = Weaver.RpcWeaver.Process(asm.MainModule);
            foreach (var d in res.Diagnostics) diagnostics.Add(new DiagnosticMessage { DiagnosticType = DiagnosticType.Warning, MessageData = "Aurix.Net weaver: " + d });
            if (!res.Changed) return new ILPostProcessResult(a.InMemoryAssembly, diagnostics);
            var pe = new MemoryStream(); var pdb = new MemoryStream();
            asm.Write(pe, new WriterParameters { SymbolWriterProvider = new PortablePdbWriterProvider(), SymbolStream = pdb, WriteSymbols = true });
            return new ILPostProcessResult(new InMemoryAssembly(pe.ToArray(), pdb.ToArray()), diagnostics);
        }
    }
}
