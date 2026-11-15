using System.IO;
using UnrealBuildTool;

/// Prebuilt libaurix_net (crates/net-ffi) and its C headers; staged by sdk/unreal/scripts/build_native.sh.
public class AurixNetLibrary : ModuleRules
{
	public AurixNetLibrary(ReadOnlyTargetRules Target) : base(Target)
	{
		Type = ModuleType.External;

		string IncludeDir = Path.Combine(ModuleDirectory, "include");
		string LibDir = Path.Combine(ModuleDirectory, "lib");
		PublicSystemIncludePaths.Add(IncludeDir);

		if (!File.Exists(Path.Combine(IncludeDir, "aurix_net.h")))
		{
			throw new BuildException(
				"AurixNetLibrary: include/aurix_net.h is missing. Run sdk/unreal/scripts/build_native.sh from the " +
				"aurix-net repository, or copy the release artifacts into " + ModuleDirectory);
		}

		if (Target.Platform == UnrealTargetPlatform.Win64)
		{
			string Dir = Path.Combine(LibDir, "Win64");
			PublicAdditionalLibraries.Add(Path.Combine(Dir, "aurix_net.dll.lib"));
			PublicDelayLoadDLLs.Add("aurix_net.dll");
			RuntimeDependencies.Add("$(BinaryOutputDir)/aurix_net.dll", Path.Combine(Dir, "aurix_net.dll"));
		}
		else if (Target.Platform == UnrealTargetPlatform.Linux)
		{
			string Lib = Path.Combine(LibDir, "Linux", "libaurix_net.so");
			PublicAdditionalLibraries.Add(Lib);
			RuntimeDependencies.Add("$(BinaryOutputDir)/libaurix_net.so", Lib);
		}
		else if (Target.Platform == UnrealTargetPlatform.Mac)
		{
			string Lib = Path.Combine(LibDir, "Mac", "libaurix_net.dylib");
			PublicAdditionalLibraries.Add(Lib);
			RuntimeDependencies.Add("$(BinaryOutputDir)/libaurix_net.dylib", Lib);
		}
		else if (Target.Platform == UnrealTargetPlatform.Android)
		{
			string Dir = Path.Combine(LibDir, "Android");
			string[] Abis = { "arm64-v8a", "armeabi-v7a", "x86_64" };
			bool Any = false;
			foreach (string Abi in Abis)
			{
				string Lib = Path.Combine(Dir, Abi, "libaurix_net.so");
				if (File.Exists(Lib)) { PublicAdditionalLibraries.Add(Lib); Any = true; }
			}
			if (!Any)
			{
				throw new BuildException("AurixNetLibrary: no prebuilt aurix_net for Android under " + Dir +
					" (run sdk/unreal/scripts/build_native.sh --target aarch64-linux-android)");
			}
			AdditionalPropertiesForReceipt.Add("AndroidPlugin", Path.Combine(ModuleDirectory, "AurixNetLibrary_APL.xml"));
		}
		else if (Target.Platform == UnrealTargetPlatform.IOS)
		{
			// iOS: static library (no dylibs in the app bundle); built with --target aarch64-apple-ios.
			string Lib = Path.Combine(LibDir, "IOS", "libaurix_net.a");
			if (!File.Exists(Lib)) throw new BuildException("AurixNetLibrary: " + Lib + " missing (run sdk/unreal/scripts/build_native.sh --target aarch64-apple-ios)");
			PublicAdditionalLibraries.Add(Lib);
			PublicFrameworks.AddRange(new string[] { "Security", "SystemConfiguration", "CoreFoundation" });
			PublicSystemLibraries.AddRange(new string[] { "resolv", "c++" });
		}
		else
		{
			throw new BuildException("AurixNetLibrary: platform " + Target.Platform + " has no prebuilt aurix_net yet");
		}
	}
}
