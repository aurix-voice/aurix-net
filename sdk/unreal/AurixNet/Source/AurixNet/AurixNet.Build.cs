using UnrealBuildTool;

public class AurixNet : ModuleRules
{
	public AurixNet(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
		// UE 5.6+ headers need C++20; never pin Cpp17 here.
		CppStandard = CppStandardVersion.Default;

		PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "NetCore" });
		PrivateDependencyModuleNames.AddRange(new string[] { "Json", "JsonUtilities", "Sockets", "PacketHandler", "Projects", "DeveloperSettings", "AurixNetLibrary" });
	}
}
