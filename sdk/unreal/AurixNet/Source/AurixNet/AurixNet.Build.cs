using UnrealBuildTool;

public class AurixNet : ModuleRules
{
	public AurixNet(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;

		PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "NetCore" });
		PrivateDependencyModuleNames.AddRange(new string[] { "Json", "Sockets", "PacketHandler", "Projects", "DeveloperSettings", "AurixNetLibrary" });
	}
}
