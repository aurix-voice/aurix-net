#include "OnlineIdentityAurix.h"
#include "OnlineSubsystemAurix.h"
#include "OnlineSubsystemTypes.h"
#include "OnlineError.h"
#include "AurixNetSubsystem.h"
#include "Engine/GameInstance.h"
#include "Engine/LocalPlayer.h"
#include "GameFramework/PlayerController.h"
#include "GameFramework/PlayerState.h"

FOnlineIdentityAurix::FOnlineIdentityAurix(FOnlineSubsystemAurix* InSubsystem)
	: Subsystem(InSubsystem)
{
}

FString FOnlineIdentityAurix::PlayerId(int32 LocalUserNum) const
{
	const UAurixNetSubsystem* N = LocalUserNum == 0 && Subsystem ? Subsystem->GetNetSubsystem() : nullptr;
	return N ? N->GetPlayerId() : FString();
}

void FOnlineIdentityAurix::WritePlayerId() const
{
	const UAurixNetSubsystem* N = Subsystem ? Subsystem->GetNetSubsystem() : nullptr;
	const UGameInstance* Game = N ? N->GetGameInstance() : nullptr;
	ULocalPlayer* Player = Game ? Game->FindLocalPlayerFromControllerId(0) : nullptr;
	if (!Player) return;
	const FUniqueNetIdRepl Repl(GetUniquePlayerId(0));
	Player->SetCachedUniqueNetId(Repl);
	APlayerController* Controller = Player->PlayerController;
	if (Controller && Controller->PlayerState) Controller->PlayerState->SetUniqueId(Repl);
}

void FOnlineIdentityAurix::Tick()
{
	const FString Now = PlayerId(0);
	if (Now == Announced) return;
	const FString Was = Announced;
	Announced = Now;
	WritePlayerId();
	if (!Was.IsEmpty())
	{
		TriggerOnLoginStatusChangedDelegates(0, ELoginStatus::LoggedIn, ELoginStatus::NotLoggedIn, *FUniqueNetIdString::Create(Was, AURIX_SUBSYSTEM));
	}
	if (!Now.IsEmpty())
	{
		TriggerOnLoginStatusChangedDelegates(0, ELoginStatus::NotLoggedIn, ELoginStatus::LoggedIn, *FUniqueNetIdString::Create(Now, AURIX_SUBSYSTEM));
	}
}

bool FOnlineIdentityAurix::Login(int32 LocalUserNum, const FOnlineAccountCredentials& AccountCredentials)
{
	if (LocalUserNum != 0)
	{
		TriggerOnLoginCompleteDelegates(LocalUserNum, false, *FUniqueNetIdString::EmptyId(), TEXT("the node connection carries one player, local user 0"));
		return false;
	}
	Tick();
	if (const FUniqueNetIdPtr Id = GetUniquePlayerId(LocalUserNum))
	{
		TriggerOnLoginCompleteDelegates(LocalUserNum, true, *Id, FString());
		return true;
	}
#if WITH_EDITOR
	if (UAurixNetSubsystem* N = Subsystem ? Subsystem->GetNetSubsystem() : nullptr)
	{
		const TWeakPtr<IOnlineIdentity, ESPMode::ThreadSafe> Alive = Subsystem->GetIdentityInterface();
		N->ConnectWithDevGrantAsync(AccountCredentials.Id, FAurixNetAsyncResult::CreateLambda([this, Alive, LocalUserNum](int32 Result, EAurixNetError, const FString& Error)
		{
			if (!Alive.IsValid()) return;
			Tick();
			const FUniqueNetIdPtr Id = Result < 0 ? FUniqueNetIdPtr() : GetUniquePlayerId(LocalUserNum);
			TriggerOnLoginCompleteDelegates(LocalUserNum, Id.IsValid(), Id.IsValid() ? *Id : *FUniqueNetIdString::EmptyId(), Error);
		}));
		return true;
	}
#endif
	TriggerOnLoginCompleteDelegates(LocalUserNum, false, *FUniqueNetIdString::EmptyId(), TEXT("Aurix Net is not connected (call Connect with a grant first)"));
	return false;
}

bool FOnlineIdentityAurix::Logout(int32 LocalUserNum)
{
	UAurixNetSubsystem* N = Subsystem ? Subsystem->GetNetSubsystem() : nullptr;
	const bool bWasLoggedIn = N && !PlayerId(LocalUserNum).IsEmpty();
	if (bWasLoggedIn)
	{
		N->Disconnect();
		Tick();
	}
	TriggerOnLogoutCompleteDelegates(LocalUserNum, bWasLoggedIn);
	return bWasLoggedIn;
}

bool FOnlineIdentityAurix::AutoLogin(int32 LocalUserNum)
{
	// the engine calls this itself when a networked game starts: it tells of the connection the game made, and makes none
	Tick();
	const FUniqueNetIdPtr Id = GetUniquePlayerId(LocalUserNum);
	TriggerOnLoginCompleteDelegates(LocalUserNum, Id.IsValid(), Id.IsValid() ? *Id : *FUniqueNetIdString::EmptyId(), Id.IsValid() ? FString() : TEXT("Aurix Net is not connected (call Connect with a grant first)"));
	return Id.IsValid();
}
TSharedPtr<FUserOnlineAccount> FOnlineIdentityAurix::GetUserAccount(const FUniqueNetId&) const { return nullptr; }
TArray<TSharedPtr<FUserOnlineAccount>> FOnlineIdentityAurix::GetAllUserAccounts() const { return {}; }
FUniqueNetIdPtr FOnlineIdentityAurix::GetUniquePlayerId(int32 LocalUserNum) const
{
	const FString Id = PlayerId(LocalUserNum);
	return Id.IsEmpty() ? FUniqueNetIdPtr() : FUniqueNetIdPtr(FUniqueNetIdString::Create(Id, AURIX_SUBSYSTEM));
}
FUniqueNetIdPtr FOnlineIdentityAurix::CreateUniquePlayerId(uint8* Bytes, int32 Size)
{
	if (!Bytes || Size <= 0) return nullptr;
	return FUniqueNetIdString::Create(FString::ConstructFromPtrSize(reinterpret_cast<const ANSICHAR*>(Bytes), Size), AURIX_SUBSYSTEM);
}
FUniqueNetIdPtr FOnlineIdentityAurix::CreateUniquePlayerId(const FString& Str) { return FUniqueNetIdString::Create(Str, AURIX_SUBSYSTEM); }
ELoginStatus::Type FOnlineIdentityAurix::GetLoginStatus(int32 LocalUserNum) const { return PlayerId(LocalUserNum).IsEmpty() ? ELoginStatus::NotLoggedIn : ELoginStatus::LoggedIn; }
ELoginStatus::Type FOnlineIdentityAurix::GetLoginStatus(const FUniqueNetId& UserId) const
{
	const FString Id = PlayerId(0);
	return !Id.IsEmpty() && Id == UserId.ToString() ? ELoginStatus::LoggedIn : ELoginStatus::NotLoggedIn;
}
FString FOnlineIdentityAurix::GetPlayerNickname(int32 LocalUserNum) const
{
	const UAurixNetSubsystem* N = LocalUserNum == 0 && Subsystem ? Subsystem->GetNetSubsystem() : nullptr;
	return N ? N->GetPlayerName() : FString();
}
FString FOnlineIdentityAurix::GetPlayerNickname(const FUniqueNetId& UserId) const
{
	const UAurixNetSubsystem* N = Subsystem ? Subsystem->GetNetSubsystem() : nullptr;
	if (!N) return FString();
	const FString Id = UserId.ToString();
	if (Id == N->GetPlayerId()) return N->GetPlayerName();
	for (const FAurixNetPlayer& Player : N->GetRoom().Players)
	{
		if (Player.PlayerId == Id) return Player.Name;
	}
	return FString();
}
FString FOnlineIdentityAurix::GetAuthToken(int32) const { return FString(); }
void FOnlineIdentityAurix::RevokeAuthToken(const FUniqueNetId& LocalUserId, const FOnRevokeAuthTokenCompleteDelegate& Delegate) { Delegate.ExecuteIfBound(LocalUserId, FOnlineError(true)); }
void FOnlineIdentityAurix::GetUserPrivilege(const FUniqueNetId& LocalUserId, EUserPrivileges::Type Privilege, const FOnGetUserPrivilegeCompleteDelegate& Delegate, EShowPrivilegeResolveUI)
{
	Delegate.ExecuteIfBound(LocalUserId, Privilege, (uint32)IOnlineIdentity::EPrivilegeResults::NoFailures);
}
FPlatformUserId FOnlineIdentityAurix::GetPlatformUserIdFromUniqueNetId(const FUniqueNetId& UniqueNetId) const
{
	return GetLoginStatus(UniqueNetId) == ELoginStatus::LoggedIn ? GetPlatformUserIdFromLocalUserNum(0) : PLATFORMUSERID_NONE;
}
