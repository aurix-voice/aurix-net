using UnrealBuildTool;

public class OnlineSubsystemAurix : ModuleRules
{
	public OnlineSubsystemAurix(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
		PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine", "OnlineSubsystem", "OnlineSubsystemUtils", "AurixNet" });
	}
}
