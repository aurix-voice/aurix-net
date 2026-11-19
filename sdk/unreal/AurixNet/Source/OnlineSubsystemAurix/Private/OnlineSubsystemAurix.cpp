#include "OnlineSubsystemAurix.h"
#include "OnlineSessionAurix.h"
#include "OnlineIdentityAurix.h"
#include "OnlineSubsystemUtils.h"
#include "AurixNetSubsystem.h"
#include "Engine/World.h"

IOnlineSessionPtr FOnlineSubsystemAurix::GetSessionInterface() const { return SessionInterface; }
IOnlineIdentityPtr FOnlineSubsystemAurix::GetIdentityInterface() const { return IdentityInterface; }
FText FOnlineSubsystemAurix::GetOnlineServiceName() const { return NSLOCTEXT("OnlineSubsystemAurix", "OnlineServiceName", "Aurix Net"); }

UAurixNetSubsystem* FOnlineSubsystemAurix::GetNetSubsystem() const
{
	return UAurixNetSubsystem::Get(GetWorldForOnline(InstanceName));
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
	if (IdentityInterface.IsValid()) IdentityInterface->Tick();
	if (SessionInterface.IsValid()) SessionInterface->Tick(DeltaTime);
	return true;
}
