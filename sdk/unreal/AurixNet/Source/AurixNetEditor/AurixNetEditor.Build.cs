using UnrealBuildTool;

public class AurixNetEditor : ModuleRules
{
	public AurixNetEditor(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;
		CppStandard = CppStandardVersion.Default;
		PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine" });
		PrivateDependencyModuleNames.AddRange(new string[] { "Slate", "SlateCore", "UnrealEd", "ToolMenus", "WorkspaceMenuStructure", "Projects", "InputCore", "Settings", "AurixNet" });
	}
}
