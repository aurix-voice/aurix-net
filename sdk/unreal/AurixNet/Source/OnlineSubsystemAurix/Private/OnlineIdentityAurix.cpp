#include "OnlineIdentityAurix.h"
#include "OnlineSubsystemAurix.h"
#include "OnlineSubsystemTypes.h"
#include "OnlineError.h"
#include "Misc/Guid.h"

FOnlineIdentityAurix::FOnlineIdentityAurix(FOnlineSubsystemAurix* InSubsystem)
	: Subsystem(InSubsystem)
{
	// The session nodes need an id for the first local player before any login call.
	Login(0, FOnlineAccountCredentials());
}

bool FOnlineIdentityAurix::Login(int32 LocalUserNum, const FOnlineAccountCredentials&)
{
	if (LocalUserNum < 0 || LocalUserNum >= MAX_LOCAL_PLAYERS)
	{
		TriggerOnLoginCompleteDelegates(LocalUserNum, false, *FUniqueNetIdString::EmptyId(), FString::Printf(TEXT("Invalid LocalUserNum=%d"), LocalUserNum));
		return false;
	}
	if (const FUniqueNetIdRef* Existing = Ids.Find(LocalUserNum))
	{
		TriggerOnLoginCompleteDelegates(LocalUserNum, true, **Existing, FString());
		return true;
	}
	const FUniqueNetIdRef Id = FUniqueNetIdString::Create(FGuid::NewGuid().ToString(EGuidFormats::Digits), AURIX_SUBSYSTEM);
	Ids.Add(LocalUserNum, Id);
	TriggerOnLoginCompleteDelegates(LocalUserNum, true, *Id, FString());
	TriggerOnLoginStatusChangedDelegates(LocalUserNum, ELoginStatus::NotLoggedIn, ELoginStatus::LoggedIn, *Id);
	return true;
}

bool FOnlineIdentityAurix::Logout(int32 LocalUserNum)
{
	FUniqueNetIdRef Id = FUniqueNetIdString::EmptyId();
	const bool bWasLoggedIn = Ids.RemoveAndCopyValue(LocalUserNum, Id);
	TriggerOnLogoutCompleteDelegates(LocalUserNum, bWasLoggedIn);
	if (bWasLoggedIn)
	{
		TriggerOnLoginStatusChangedDelegates(LocalUserNum, ELoginStatus::LoggedIn, ELoginStatus::NotLoggedIn, *Id);
	}
	return bWasLoggedIn;
}

bool FOnlineIdentityAurix::AutoLogin(int32 LocalUserNum) { return Login(LocalUserNum, FOnlineAccountCredentials()); }
TSharedPtr<FUserOnlineAccount> FOnlineIdentityAurix::GetUserAccount(const FUniqueNetId&) const { return nullptr; }
TArray<TSharedPtr<FUserOnlineAccount>> FOnlineIdentityAurix::GetAllUserAccounts() const { return {}; }
FUniqueNetIdPtr FOnlineIdentityAurix::GetUniquePlayerId(int32 LocalUserNum) const
{
	const FUniqueNetIdRef* Found = Ids.Find(LocalUserNum);
	return Found ? FUniqueNetIdPtr(*Found) : FUniqueNetIdPtr();
}
FUniqueNetIdPtr FOnlineIdentityAurix::CreateUniquePlayerId(uint8* Bytes, int32 Size)
{
	if (!Bytes || Size <= 0) return nullptr;
	return FUniqueNetIdString::Create(FString::ConstructFromPtrSize(reinterpret_cast<const ANSICHAR*>(Bytes), Size), AURIX_SUBSYSTEM);
}
FUniqueNetIdPtr FOnlineIdentityAurix::CreateUniquePlayerId(const FString& Str) { return FUniqueNetIdString::Create(Str, AURIX_SUBSYSTEM); }
ELoginStatus::Type FOnlineIdentityAurix::GetLoginStatus(int32 LocalUserNum) const { return Ids.Contains(LocalUserNum) ? ELoginStatus::LoggedIn : ELoginStatus::NotLoggedIn; }
ELoginStatus::Type FOnlineIdentityAurix::GetLoginStatus(const FUniqueNetId& UserId) const
{
	for (const TPair<int32, FUniqueNetIdRef>& User : Ids)
	{
		if (*User.Value == UserId)
		{
			return ELoginStatus::LoggedIn;
		}
	}
	return ELoginStatus::NotLoggedIn;
}
FString FOnlineIdentityAurix::GetPlayerNickname(int32 LocalUserNum) const { return FString::Printf(TEXT("Player%d"), LocalUserNum); }
FString FOnlineIdentityAurix::GetPlayerNickname(const FUniqueNetId& UserId) const { return UserId.ToString().Left(8); }
FString FOnlineIdentityAurix::GetAuthToken(int32) const { return FString(); }
void FOnlineIdentityAurix::RevokeAuthToken(const FUniqueNetId& LocalUserId, const FOnRevokeAuthTokenCompleteDelegate& Delegate) { Delegate.ExecuteIfBound(LocalUserId, FOnlineError(true)); }
void FOnlineIdentityAurix::GetUserPrivilege(const FUniqueNetId& LocalUserId, EUserPrivileges::Type Privilege, const FOnGetUserPrivilegeCompleteDelegate& Delegate, EShowPrivilegeResolveUI)
{
	Delegate.ExecuteIfBound(LocalUserId, Privilege, (uint32)IOnlineIdentity::EPrivilegeResults::NoFailures);
}
FPlatformUserId FOnlineIdentityAurix::GetPlatformUserIdFromUniqueNetId(const FUniqueNetId& UniqueNetId) const
{
	for (const TPair<int32, FUniqueNetIdRef>& User : Ids)
	{
		if (*User.Value == UniqueNetId)
		{
			return GetPlatformUserIdFromLocalUserNum(User.Key);
		}
	}
	return PLATFORMUSERID_NONE;
}
