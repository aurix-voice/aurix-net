#pragma once
#include "CoreMinimal.h"
#include "OnlineSubsystemImpl.h"
#include "OnlineSubsystemAurixPackage.h"

/** Subsystem name for DefaultEngine.ini: [OnlineSubsystem] DefaultPlatformService=Aurix */
#define AURIX_SUBSYSTEM FName(TEXT("Aurix"))

class FOnlineSessionAurix;
class FOnlineIdentityAurix;
typedef TSharedPtr<FOnlineSessionAurix, ESPMode::ThreadSafe> FOnlineSessionAurixPtr;
typedef TSharedPtr<FOnlineIdentityAurix, ESPMode::ThreadSafe> FOnlineIdentityAurixPtr;

/**
 * Online Subsystem over Aurix Net rooms: Create Session = create / join a relay room, Find Sessions =
 * list rooms, Join Session = join a room; the resolved connect string is `aurix://<room>` for the
 * AurixNetDriver. Identity is a thin local id so the standard session nodes work.
 */
class ONLINESUBSYSTEMAURIX_API FOnlineSubsystemAurix : public FOnlineSubsystemImpl
{
public:
	explicit FOnlineSubsystemAurix(FName InInstanceName) : FOnlineSubsystemImpl(AURIX_SUBSYSTEM, InInstanceName) {}
	virtual ~FOnlineSubsystemAurix() = default;

	virtual IOnlineSessionPtr GetSessionInterface() const override;
	virtual IOnlineIdentityPtr GetIdentityInterface() const override;
	virtual IOnlineFriendsPtr GetFriendsInterface() const override { return nullptr; }
	virtual IOnlinePartyPtr GetPartyInterface() const override { return nullptr; }
	virtual IOnlineGroupsPtr GetGroupsInterface() const override { return nullptr; }
	virtual IOnlineSharedCloudPtr GetSharedCloudInterface() const override { return nullptr; }
	virtual IOnlineUserCloudPtr GetUserCloudInterface() const override { return nullptr; }
	virtual IOnlineEntitlementsPtr GetEntitlementsInterface() const override { return nullptr; }
	virtual IOnlineLeaderboardsPtr GetLeaderboardsInterface() const override { return nullptr; }
	virtual IOnlineVoicePtr GetVoiceInterface() const override { return nullptr; }
	virtual IOnlineExternalUIPtr GetExternalUIInterface() const override { return nullptr; }
	virtual IOnlineTimePtr GetTimeInterface() const override { return nullptr; }
	virtual IOnlineTitleFilePtr GetTitleFileInterface() const override { return nullptr; }
	virtual IOnlineStoreV2Ptr GetStoreV2Interface() const override { return nullptr; }
	virtual IOnlinePurchasePtr GetPurchaseInterface() const override { return nullptr; }
	virtual IOnlineEventsPtr GetEventsInterface() const override { return nullptr; }
	virtual IOnlineAchievementsPtr GetAchievementsInterface() const override { return nullptr; }
	virtual IOnlineSharingPtr GetSharingInterface() const override { return nullptr; }
	virtual IOnlineUserPtr GetUserInterface() const override { return nullptr; }
	virtual IOnlineMessagePtr GetMessageInterface() const override { return nullptr; }
	virtual IOnlinePresencePtr GetPresenceInterface() const override { return nullptr; }
	virtual IOnlineChatPtr GetChatInterface() const override { return nullptr; }
	virtual IOnlineStatsPtr GetStatsInterface() const override { return nullptr; }
	virtual IOnlineTurnBasedPtr GetTurnBasedInterface() const override { return nullptr; }
	virtual IOnlineTournamentPtr GetTournamentInterface() const override { return nullptr; }

	virtual bool Init() override;
	virtual bool Shutdown() override;
	virtual bool Tick(float DeltaTime) override;
	virtual FString GetAppId() const override { return TEXT("aurix"); }
	virtual FText GetOnlineServiceName() const override;
	virtual bool Exec(UWorld* InWorld, const TCHAR* Cmd, FOutputDevice& Ar) override;

	/** The game instance's Aurix Net subsystem for this OSS instance (PIE-aware), or null. */
	class UAurixNetSubsystem* GetNetSubsystem() const;

private:
	FOnlineSessionAurixPtr SessionInterface;
	FOnlineIdentityAurixPtr IdentityInterface;
};

typedef TSharedPtr<FOnlineSubsystemAurix, ESPMode::ThreadSafe> FOnlineSubsystemAurixPtr;
