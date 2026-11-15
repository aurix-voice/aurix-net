#include "AurixNetLog.h"
#include "AurixNetDriver.h"
#include "Modules/ModuleManager.h"
#include "Interfaces/IPluginManager.h"
#include "HAL/PlatformProcess.h"
#include "Misc/Paths.h"
#include "HAL/IConsoleManager.h"
#include "AurixNetSubsystem.h"
#include "Engine/Engine.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"

static UAurixNetSubsystem* AurixNetFromWorld(UWorld* W) { UGameInstance* GI = W ? W->GetGameInstance() : nullptr; return GI ? GI->GetSubsystem<UAurixNetSubsystem>() : nullptr; }

static FAutoConsoleCommandWithWorldAndArgs GAurixNetStatus(TEXT("aurix.net.status"), TEXT("Aurix Net: connection, room, players"),
	FConsoleCommandWithWorldAndArgsDelegate::CreateStatic([](const TArray<FString>&, UWorld* W)
	{
		UAurixNetSubsystem* N = AurixNetFromWorld(W);
		if (!N) { UE_LOG(LogAurixNet, Display, TEXT("no Aurix Net subsystem")); return; }
		const FAurixNetRoomInfo& R = N->GetRoom();
		UE_LOG(LogAurixNet, Display, TEXT("state=%d native=%s room='%s' (%s) mode=%d actor=%d master=%d players=%d/%d"), (int32)N->GetState(), *UAurixNetSubsystem::GetNativeVersion(), *R.Name, *R.RoomId, (int32)R.Mode, R.Actor, R.Master, R.Players.Num(), R.MaxPlayers);
		for (const FAurixNetPlayer& P : R.Players) UE_LOG(LogAurixNet, Display, TEXT("  actor %d  %s  '%s'%s"), P.Actor, *P.PlayerId, *P.Name, P.bOnline ? TEXT("") : TEXT("  (offline)"));
	}));
static FAutoConsoleCommandWithWorldAndArgs GAurixNetRooms(TEXT("aurix.net.rooms"), TEXT("Aurix Net: request the cell's room list (logged on arrival)"),
	FConsoleCommandWithWorldAndArgsDelegate::CreateStatic([](const TArray<FString>& A, UWorld* W)
	{
		UAurixNetSubsystem* N = AurixNetFromWorld(W); if (!N) return;
		N->OnRoomsNative.AddLambda([](const TArray<FAurixNetRoomSummary>& Rooms) { for (const FAurixNetRoomSummary& R : Rooms) UE_LOG(LogAurixNet, Display, TEXT("  %s  %s  %d/%d  %s  %s"), *R.Name, *R.RoomId, R.Players, R.MaxPlayers, R.bOpen ? TEXT("open") : TEXT("closed"), *R.PropsJson); });
		N->RequestRooms(A.Num() ? A[0] : FString(), 50);
	}));
static FAutoConsoleCommandWithWorldAndArgs GAurixNetLeave(TEXT("aurix.net.leave"), TEXT("Aurix Net: leave the room"),
	FConsoleCommandWithWorldAndArgsDelegate::CreateStatic([](const TArray<FString>&, UWorld* W) { if (UAurixNetSubsystem* N = AurixNetFromWorld(W)) N->Leave(); }));

DEFINE_LOG_CATEGORY(LogAurixNet);

class FAurixNetModule : public IModuleInterface
{
public:
	virtual void StartupModule() override
	{
#if PLATFORM_WINDOWS
		const TSharedPtr<IPlugin> Plugin = IPluginManager::Get().FindPlugin(TEXT("AurixNet"));
		if (Plugin.IsValid())
		{
			const FString Dll = FPaths::Combine(Plugin->GetBaseDir(), TEXT("Source/ThirdParty/AurixNetLibrary/lib/Win64/aurix_net.dll"));
			LibHandle = FPlatformProcess::GetDllHandle(*Dll);
			if (!LibHandle) { UE_LOG(LogAurixNet, Warning, TEXT("aurix_net.dll not found at %s; relying on the packaged binary"), *Dll); }
		}
#endif
		UE_LOG(LogAurixNet, Log, TEXT("Aurix Net plugin loaded"));
	}
	virtual void ShutdownModule() override
	{
		if (LibHandle) { FPlatformProcess::FreeDllHandle(LibHandle); LibHandle = nullptr; }
	}
private:
	void* LibHandle = nullptr;
};

IMPLEMENT_MODULE(FAurixNetModule, AurixNet)
