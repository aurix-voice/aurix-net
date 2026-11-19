#pragma once
#include "CoreMinimal.h"
#include "Interfaces/OnlineIdentityInterface.h"
#include "OnlineSubsystemAurixPackage.h"

class FOnlineSubsystemAurix;

/** The identity of the node connection: the first local user is the player the grant names, by the
 *  id the node knows it by, from the moment the plugin is connected until it is not. */
class FOnlineIdentityAurix : public IOnlineIdentity
{
public:
	explicit FOnlineIdentityAurix(FOnlineSubsystemAurix* InSubsystem);
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

	/** Tells the login status delegates of a connection made or lost since the last call. */
	void Tick();

private:
	FString PlayerId(int32 LocalUserNum) const;
	/** The engine reads a player's id from the local player and its player state, and writes it there only when the controller logs in. */
	void WritePlayerId() const;

	FOnlineSubsystemAurix* Subsystem;
	/** The player id the login status delegates were last told of; empty when not logged in. */
	FString Announced;
};
