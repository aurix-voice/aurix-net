#include "OnlineIdentityAurix.h"
#include "OnlineSubsystemAurix.h"
#include "OnlineSubsystemTypes.h"
#include "OnlineError.h"
#include "Misc/Guid.h"

FUniqueNetIdPtr FOnlineIdentityAurix::IdFor(int32 LocalUserNum) const
{
	if (FUniqueNetIdPtr* Found = Ids.Find(LocalUserNum)) return *Found;
	const FUniqueNetIdPtr Id = FUniqueNetIdString::Create(FGuid::NewGuid().ToString(EGuidFormats::Digits), AURIX_SUBSYSTEM);
	Ids.Add(LocalUserNum, Id);
	return Id;
}

bool FOnlineIdentityAurix::Login(int32 LocalUserNum, const FOnlineAccountCredentials&)
{
	const FUniqueNetIdPtr Id = IdFor(LocalUserNum);
	TriggerOnLoginCompleteDelegates(LocalUserNum, true, *Id, FString());
	TriggerOnLoginStatusChangedDelegates(LocalUserNum, ELoginStatus::NotLoggedIn, ELoginStatus::LoggedIn, *Id);
	return true;
}
bool FOnlineIdentityAurix::Logout(int32 LocalUserNum) { TriggerOnLogoutCompleteDelegates(LocalUserNum, true); return true; }
bool FOnlineIdentityAurix::AutoLogin(int32 LocalUserNum) { return Login(LocalUserNum, FOnlineAccountCredentials()); }
TSharedPtr<FUserOnlineAccount> FOnlineIdentityAurix::GetUserAccount(const FUniqueNetId&) const { return nullptr; }
TArray<TSharedPtr<FUserOnlineAccount>> FOnlineIdentityAurix::GetAllUserAccounts() const { return {}; }
FUniqueNetIdPtr FOnlineIdentityAurix::GetUniquePlayerId(int32 LocalUserNum) const { return IdFor(LocalUserNum); }
FUniqueNetIdPtr FOnlineIdentityAurix::CreateUniquePlayerId(uint8* Bytes, int32 Size)
{
	if (!Bytes || Size <= 0) return nullptr;
	return FUniqueNetIdString::Create(FString::ConstructFromPtrSize(reinterpret_cast<const ANSICHAR*>(Bytes), Size), AURIX_SUBSYSTEM);
}
FUniqueNetIdPtr FOnlineIdentityAurix::CreateUniquePlayerId(const FString& Str) { return FUniqueNetIdString::Create(Str, AURIX_SUBSYSTEM); }
ELoginStatus::Type FOnlineIdentityAurix::GetLoginStatus(int32 LocalUserNum) const { return Ids.Contains(LocalUserNum) ? ELoginStatus::LoggedIn : ELoginStatus::NotLoggedIn; }
ELoginStatus::Type FOnlineIdentityAurix::GetLoginStatus(const FUniqueNetId& UserId) const { return UserId.IsValid() ? ELoginStatus::LoggedIn : ELoginStatus::NotLoggedIn; }
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
	for (const auto& P : Ids) if (P.Value.IsValid() && *P.Value == UniqueNetId) return GetPlatformUserIdFromLocalUserNum(P.Key);
	return PLATFORMUSERID_NONE;
}
