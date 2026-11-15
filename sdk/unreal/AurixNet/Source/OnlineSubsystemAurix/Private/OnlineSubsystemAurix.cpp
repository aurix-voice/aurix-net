#include "OnlineSubsystemAurix.h"
#include "OnlineSessionAurix.h"
#include "OnlineIdentityAurix.h"
#include "OnlineSubsystemUtils.h"
#include "AurixNetSubsystem.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"

IOnlineSessionPtr FOnlineSubsystemAurix::GetSessionInterface() const { return SessionInterface; }
IOnlineIdentityPtr FOnlineSubsystemAurix::GetIdentityInterface() const { return IdentityInterface; }
FText FOnlineSubsystemAurix::GetOnlineServiceName() const { return NSLOCTEXT("OnlineSubsystemAurix", "OnlineServiceName", "Aurix Net"); }

UAurixNetSubsystem* FOnlineSubsystemAurix::GetNetSubsystem() const
{
	const UWorld* W = GetWorldForOnline(InstanceName);
	const UGameInstance* GI = W ? W->GetGameInstance() : nullptr;
	return GI ? GI->GetSubsystem<UAurixNetSubsystem>() : nullptr;
}

bool FOnlineSubsystemAurix::Init()
{
	SessionInterface = MakeShared<FOnlineSessionAurix, ESPMode::ThreadSafe>(this);
	IdentityInterface = MakeShared<FOnlineIdentityAurix, ESPMode::ThreadSafe>(this);
	return true;
}
bool FOnlineSubsystemAurix::Shutdown()
{
	FOnlineSubsystemImpl::Shutdown();
	SessionInterface.Reset();
	IdentityInterface.Reset();
	return true;
}
bool FOnlineSubsystemAurix::Tick(float DeltaTime)
{
	if (!FOnlineSubsystemImpl::Tick(DeltaTime)) return false;
	if (SessionInterface.IsValid()) SessionInterface->Tick(DeltaTime);
	return true;
}
bool FOnlineSubsystemAurix::Exec(UWorld* InWorld, const TCHAR* Cmd, FOutputDevice& Ar) { return FOnlineSubsystemImpl::Exec(InWorld, Cmd, Ar); }
