using UnrealBuildTool;

public class AurixNetEditor : ModuleRules
{
	public AurixNetEditor(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
		PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine" });
		PrivateDependencyModuleNames.AddRange(new string[] { "Slate", "SlateCore", "UnrealEd", "WorkspaceMenuStructure", "Settings", "DeveloperSettings", "AurixNet" });
	}
}
