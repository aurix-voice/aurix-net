#pragma once
#include "CoreMinimal.h"
#include "Interfaces/OnlineIdentityInterface.h"
#include "OnlineSubsystemAurixPackage.h"

class FOnlineSubsystemAurix;

/** Local-only identity: one id per local user so session nodes have a valid FUniqueNetId. The real
 *  player identity is the grant your backend minted; read it from UAurixNetSubsystem::GetRoom(). */
class FOnlineIdentityAurix : public IOnlineIdentity
{
public:
	explicit FOnlineIdentityAurix(FOnlineSubsystemAurix* InSubsystem) : Subsystem(InSubsystem) {}
	virtual ~FOnlineIdentityAurix() = default;

	virtual bool Login(int32 LocalUserNum, const FOnlineAccountCredentials& AccountCredentials) override;
	virtual bool Logout(int32 LocalUserNum) override;
	virtual bool AutoLogin(int32 LocalUserNum) override;
	virtual TSharedPtr<FUserOnlineAccount> GetUserAccount(const FUniqueNetId& UserId) const override;
	virtual TArray<TSharedPtr<FUserOnlineAccount>> GetAllUserAccounts() const override;
	virtual FUniqueNetIdPtr GetUniquePlayerId(int32 LocalUserNum) const override;
	virtual FUniqueNetIdPtr CreateUniquePlayerId(uint8* Bytes, int32 Size) override;
	virtual FUniqueNetIdPtr CreateUniquePlayerId(const FString& Str) override;
	virtual ELoginStatus::Type GetLoginStatus(int32 LocalUserNum) const override;
	virtual ELoginStatus::Type GetLoginStatus(const FUniqueNetId& UserId) const override;
	virtual FString GetPlayerNickname(int32 LocalUserNum) const override;
	virtual FString GetPlayerNickname(const FUniqueNetId& UserId) const override;
	virtual FString GetAuthToken(int32 LocalUserNum) const override;
	virtual void RevokeAuthToken(const FUniqueNetId& LocalUserId, const FOnRevokeAuthTokenCompleteDelegate& Delegate) override;
	virtual void GetUserPrivilege(const FUniqueNetId& LocalUserId, EUserPrivileges::Type Privilege, const FOnGetUserPrivilegeCompleteDelegate& Delegate, EShowPrivilegeResolveUI ShowResolveUI = EShowPrivilegeResolveUI::Default) override;
	virtual FPlatformUserId GetPlatformUserIdFromUniqueNetId(const FUniqueNetId& UniqueNetId) const override;
	virtual FString GetAuthType() const override { return TEXT("aurix"); }

private:
	FUniqueNetIdPtr IdFor(int32 LocalUserNum) const;
	FOnlineSubsystemAurix* Subsystem;
	mutable TMap<int32, FUniqueNetIdPtr> Ids;
};
