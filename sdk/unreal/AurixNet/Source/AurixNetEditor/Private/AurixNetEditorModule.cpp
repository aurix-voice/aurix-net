#include "Modules/ModuleManager.h"
#include "Editor.h"
#include "Engine/Engine.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "ToolMenus.h"
#include "Framework/Docking/TabManager.h"
#include "Widgets/Docking/SDockTab.h"
#include "Widgets/SBoxPanel.h"
#include "Widgets/Text/STextBlock.h"
#include "Widgets/Input/SButton.h"
#include "Widgets/Layout/SScrollBox.h"
#include "WorkspaceMenuStructure.h"
#include "WorkspaceMenuStructureModule.h"
#include "AurixNetSubsystem.h"
#include "AurixLockstepComponent.h"
#include "EngineUtils.h"
#include "ISettingsModule.h"
#include "Framework/Application/SlateApplication.h"

#define LOCTEXT_NAMESPACE "AurixNetEditor"

static const FName AurixNetDebuggerTab(TEXT("AurixNetDebugger"));

/** Live view of every running world's Aurix Net subsystem (PIE instances, standalone game windows). */
class SAurixNetDebugger : public SCompoundWidget
{
public:
	SLATE_BEGIN_ARGS(SAurixNetDebugger) {}
	SLATE_END_ARGS()

	void Construct(const FArguments&)
	{
		ChildSlot
		[
			SNew(SVerticalBox)
			+ SVerticalBox::Slot().AutoHeight().Padding(6)
			[
				SNew(SHorizontalBox)
				+ SHorizontalBox::Slot().AutoWidth().Padding(2) [ SNew(SButton).Text(LOCTEXT("Rooms", "Request rooms")).OnClicked(this, &SAurixNetDebugger::OnRooms) ]
				+ SHorizontalBox::Slot().AutoWidth().Padding(2) [ SNew(SButton).Text(LOCTEXT("Stats", "Request stats")).OnClicked(this, &SAurixNetDebugger::OnStats) ]
				+ SHorizontalBox::Slot().AutoWidth().Padding(2) [ SNew(SButton).Text(LOCTEXT("Leave", "Leave rooms")).OnClicked(this, &SAurixNetDebugger::OnLeave) ]
				+ SHorizontalBox::Slot().AutoWidth().Padding(2) [ SNew(SButton).Text(LOCTEXT("Settings", "Project Settings…")).OnClicked(this, &SAurixNetDebugger::OnSettings) ]
			]
			+ SVerticalBox::Slot().FillHeight(1).Padding(6)
			[
				SNew(SScrollBox)
				+ SScrollBox::Slot() [ SNew(STextBlock).Text(this, &SAurixNetDebugger::Report).AutoWrapText(true) ]
			]
		];
	}

private:
	static void ForEachNet(TFunctionRef<void(const FWorldContext&, UAurixNetSubsystem*)> Fn)
	{
		if (!GEngine) return;
		for (const FWorldContext& Ctx : GEngine->GetWorldContexts())
		{
			if (!Ctx.World() || (Ctx.WorldType != EWorldType::PIE && Ctx.WorldType != EWorldType::Game)) continue;
			UGameInstance* GI = Ctx.World()->GetGameInstance();
			if (UAurixNetSubsystem* N = GI ? GI->GetSubsystem<UAurixNetSubsystem>() : nullptr) Fn(Ctx, N);
		}
	}
	static const TCHAR* StateName(EAurixNetState S)
	{
		switch (S) { case EAurixNetState::Connecting: return TEXT("connecting"); case EAurixNetState::Connected: return TEXT("connected"); case EAurixNetState::InRoom: return TEXT("in room"); case EAurixNetState::Closed: return TEXT("closed"); default: return TEXT("disconnected"); }
	}
	FText Report() const
	{
		FString S;
		int32 Count = 0;
		ForEachNet([&](const FWorldContext& Ctx, UAurixNetSubsystem* N)
		{
			++Count;
			const FAurixNetRoomInfo& R = N->GetRoom();
			S += FString::Printf(TEXT("[%s %d]  %s%s\n"), Ctx.WorldType == EWorldType::PIE ? TEXT("PIE") : TEXT("Game"), Ctx.PIEInstance, StateName(N->GetState()), N->IsBusy() ? TEXT(" (busy)") : TEXT(""));
			if (N->GetState() == EAurixNetState::InRoom)
			{
				S += FString::Printf(TEXT("  room '%s'  mode %d  actor %d  master %d%s  players %d/%d\n"), *R.Name, (int32)R.Mode, R.Actor, R.Master, N->IsMaster() ? TEXT(" (us)") : TEXT(""), R.Players.Num(), R.MaxPlayers);
				for (const FAurixNetPlayer& P : R.Players) S += FString::Printf(TEXT("    %d  %s  '%s'%s\n"), P.Actor, *P.PlayerId, *P.Name, P.bOnline ? TEXT("") : TEXT("  offline"));
				if (!N->GetRoomPropsJson().IsEmpty()) S += FString::Printf(TEXT("  props %s\n"), *N->GetRoomPropsJson());
				for (TActorIterator<AActor> It(Ctx.World()); It; ++It)
				{
					if (UAurixLockstepComponent* L = It->FindComponentByClass<UAurixLockstepComponent>())
						S += FString::Printf(TEXT("  lockstep %s: confirmed %lld  server %lld  delay %lld (adaptive %lld)  late %lld  dropped %lld  catch-ups %lld\n"), *It->GetName(), L->GetStat(7), L->GetStat(8), L->GetStat(0), L->GetStat(3), L->GetStat(6), L->GetStat(11), L->GetStat(10));
				}
			}
			const FString Err = N->GetLastError();
			if (!Err.IsEmpty()) S += FString::Printf(TEXT("  last error: %s\n"), *Err);
			S += TEXT("\n");
		});
		if (Count == 0) S = TEXT("No running game world. Start PIE; each instance's Aurix Net subsystem appears here.");
		return FText::FromString(S);
	}
	FReply OnRooms() { ForEachNet([](const FWorldContext&, UAurixNetSubsystem* N) { N->RequestRooms(); }); return FReply::Handled(); }
	FReply OnStats() { ForEachNet([](const FWorldContext&, UAurixNetSubsystem* N) { N->RequestStats(); }); return FReply::Handled(); }
	FReply OnLeave() { ForEachNet([](const FWorldContext&, UAurixNetSubsystem* N) { if (N->GetState() == EAurixNetState::InRoom) N->Leave(); }); return FReply::Handled(); }
	FReply OnSettings()
	{
		if (ISettingsModule* S = FModuleManager::GetModulePtr<ISettingsModule>("Settings")) S->ShowViewer("Project", "Plugins", "Aurix Net");
		return FReply::Handled();
	}
};

class FAurixNetEditorModule : public IModuleInterface
{
public:
	virtual void StartupModule() override
	{
		FGlobalTabmanager::Get()->RegisterNomadTabSpawner(AurixNetDebuggerTab, FOnSpawnTab::CreateLambda([](const FSpawnTabArgs&)
		{
			return SNew(SDockTab).TabRole(ETabRole::NomadTab) [ SNew(SAurixNetDebugger) ];
		}))
		.SetDisplayName(LOCTEXT("TabTitle", "Aurix Net Debugger"))
		.SetTooltipText(LOCTEXT("TabTooltip", "Connection, room, players and lockstep statistics of every running Aurix Net client"))
		.SetGroup(WorkspaceMenu::GetMenuStructure().GetDeveloperToolsDebugCategory());
	}
	virtual void ShutdownModule() override
	{
		if (FSlateApplication::IsInitialized()) FGlobalTabmanager::Get()->UnregisterNomadTabSpawner(AurixNetDebuggerTab);
	}
};

#undef LOCTEXT_NAMESPACE

IMPLEMENT_MODULE(FAurixNetEditorModule, AurixNetEditor)
