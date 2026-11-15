using UnrealBuildTool;

public class OnlineSubsystemAurix : ModuleRules
{
	public OnlineSubsystemAurix(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
		CppStandard = CppStandardVersion.Default;
		PublicDefinitions.Add("ONLINESUBSYSTEMAURIX_PACKAGE=1");
		PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "OnlineSubsystem", "OnlineSubsystemUtils", "AurixNet" });
		PrivateDependencyModuleNames.AddRange(new string[] { "Json", "Sockets" });
	}
}
