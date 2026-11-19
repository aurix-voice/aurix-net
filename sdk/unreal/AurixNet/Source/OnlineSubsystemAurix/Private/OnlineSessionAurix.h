#pragma once
#include "CoreMinimal.h"
#include "Interfaces/OnlineSessionInterface.h"
#include "OnlineSubsystemTypes.h"
#include "OnlineSessionSettings.h"
#include "OnlineSubsystemAurix.h"
#include "OnlineSubsystemAurixPackage.h"
#include "AurixNetTypes.h"

class FOnlineSubsystemAurix;
class UAurixNetSubsystem;

/** Session setting key: room name (defaults to the session name). */
#define SETTING_AURIX_ROOM FName(TEXT("AURIXROOM"))
/** Session setting key: player ids, comma separated, whose seats the room holds when this session creates it. */
#define SETTING_AURIX_RESERVED FName(TEXT("AURIXRESERVED"))

class FOnlineSessionInfoAurix : public FOnlineSessionInfo
{
public:
	FOnlineSessionInfoAurix(const FString& InRoomName, const FString& InRoomId)
		: RoomName(InRoomName), SessionId(FUniqueNetIdString::Create(InRoomId.IsEmpty() ? InRoomName : InRoomId, AURIX_SUBSYSTEM)) {}
	FString RoomName;
	FUniqueNetIdRef SessionId;
	virtual const uint8* GetBytes() const override { return nullptr; }
	virtual int32 GetSize() const override { return sizeof(FOnlineSessionInfoAurix); }
	virtual bool IsValid() const override { return !RoomName.IsEmpty(); }
	virtual FString ToString() const override { return RoomName; }
	virtual FString ToDebugString() const override { return FString::Printf(TEXT("aurix://%s"), *RoomName); }
	virtual const FUniqueNetId& GetSessionId() const override { return *SessionId; }
};

/** Sessions as Aurix relay rooms. Requests to the cell run off the game thread and complete on it. */
class FOnlineSessionAurix : public IOnlineSession, public TSharedFromThis<FOnlineSessionAurix, ESPMode::ThreadSafe>
{
public:
	explicit FOnlineSessionAurix(FOnlineSubsystemAurix* InSubsystem) : Subsystem(InSubsystem) {}
	virtual ~FOnlineSessionAurix();
	void Tick(float DeltaTime);

	// IOnlineSession
	virtual FUniqueNetIdPtr CreateSessionIdFromString(const FString& SessionIdStr) override;
	virtual FNamedOnlineSession* GetNamedSession(FName SessionName) override;
	virtual void RemoveNamedSession(FName SessionName) override;
	virtual bool HasPresenceSession() override;
	virtual EOnlineSessionState::Type GetSessionState(FName SessionName) const override;
	virtual bool CreateSession(int32 HostingPlayerNum, FName SessionName, const FOnlineSessionSettings& NewSessionSettings) override;
	virtual bool CreateSession(const FUniqueNetId& HostingPlayerId, FName SessionName, const FOnlineSessionSettings& NewSessionSettings) override;
	virtual bool StartSession(FName SessionName) override;
	virtual bool UpdateSession(FName SessionName, FOnlineSessionSettings& UpdatedSessionSettings, bool bShouldRefreshOnlineData = true) override;
	virtual bool EndSession(FName SessionName) override;
	virtual bool DestroySession(FName SessionName, const FOnDestroySessionCompleteDelegate& CompletionDelegate = FOnDestroySessionCompleteDelegate()) override;
	virtual bool IsPlayerInSession(FName SessionName, const FUniqueNetId& UniqueId) override;
	virtual bool StartMatchmaking(const TArray<FUniqueNetIdRef>& LocalPlayers, FName SessionName, const FOnlineSessionSettings& NewSessionSettings, TSharedRef<FOnlineSessionSearch>& SearchSettings) override;
	virtual bool StartMatchmaking(const TArray<FSessionMatchmakingUser>& LocalPlayers, FName SessionName, const FOnlineSessionSettings& NewSessionSettings, TSharedRef<FOnlineSessionSearch>& SearchSettings, const FOnStartMatchmakingComplete& CompletionDelegate) override;
	virtual bool CancelMatchmaking(int32 SearchingPlayerNum, FName SessionName) override;
	virtual bool CancelMatchmaking(const FUniqueNetId& SearchingPlayerId, FName SessionName) override;
	virtual bool FindSessions(int32 SearchingPlayerNum, const TSharedRef<FOnlineSessionSearch>& SearchSettings) override;
	virtual bool FindSessions(const FUniqueNetId& SearchingPlayerId, const TSharedRef<FOnlineSessionSearch>& SearchSettings) override;
	virtual bool FindSessionById(const FUniqueNetId& SearchingUserId, const FUniqueNetId& SessionId, const FUniqueNetId& FriendId, const FOnSingleSessionResultCompleteDelegate& CompletionDelegate) override;
	virtual bool CancelFindSessions() override;
	virtual bool PingSearchResults(const FOnlineSessionSearchResult& SearchResult) override;
	virtual bool JoinSession(int32 PlayerNum, FName SessionName, const FOnlineSessionSearchResult& DesiredSession) override;
	virtual bool JoinSession(const FUniqueNetId& PlayerId, FName SessionName, const FOnlineSessionSearchResult& DesiredSession) override;
	virtual bool FindFriendSession(int32 LocalUserNum, const FUniqueNetId& Friend) override;
	virtual bool FindFriendSession(const FUniqueNetId& LocalUserId, const FUniqueNetId& Friend) override;
	virtual bool FindFriendSession(const FUniqueNetId& LocalUserId, const TArray<FUniqueNetIdRef>& FriendList) override;
	virtual bool SendSessionInviteToFriend(int32 LocalUserNum, FName SessionName, const FUniqueNetId& Friend) override;
	virtual bool SendSessionInviteToFriend(const FUniqueNetId& LocalUserId, FName SessionName, const FUniqueNetId& Friend) override;
	virtual bool SendSessionInviteToFriends(int32 LocalUserNum, FName SessionName, const TArray<FUniqueNetIdRef>& Friends) override;
	virtual bool SendSessionInviteToFriends(const FUniqueNetId& LocalUserId, FName SessionName, const TArray<FUniqueNetIdRef>& Friends) override;
	virtual bool GetResolvedConnectString(FName SessionName, FString& ConnectInfo, FName PortType = NAME_GamePort) override;
	virtual bool GetResolvedConnectString(const FOnlineSessionSearchResult& SearchResult, FName PortType, FString& ConnectInfo) override;
	virtual FOnlineSessionSettings* GetSessionSettings(FName SessionName) override;
	virtual bool RegisterPlayer(FName SessionName, const FUniqueNetId& PlayerId, bool bWasInvited) override;
	virtual bool RegisterPlayers(FName SessionName, const TArray<FUniqueNetIdRef>& Players, bool bWasInvited = false) override;
	virtual bool UnregisterPlayer(FName SessionName, const FUniqueNetId& PlayerId) override;
	virtual bool UnregisterPlayers(FName SessionName, const TArray<FUniqueNetIdRef>& Players) override;
	virtual void RegisterLocalPlayer(const FUniqueNetId& PlayerId, FName SessionName, const FOnRegisterLocalPlayerCompleteDelegate& Delegate) override;
	virtual void UnregisterLocalPlayer(const FUniqueNetId& PlayerId, FName SessionName, const FOnUnregisterLocalPlayerCompleteDelegate& Delegate) override;
	virtual void RemovePlayerFromSession(int32 LocalUserNum, FName SessionName, const FUniqueNetId& TargetPlayerId) override;
	virtual int32 GetNumSessions() override;
	virtual void DumpSessionState() override;

private:
	FNamedOnlineSession* AddNamedSession(FName SessionName, const FOnlineSessionSettings& SessionSettings) override;
	FNamedOnlineSession* AddNamedSession(FName SessionName, const FOnlineSession& Session) override;
	UAurixNetSubsystem* Net() const;
	FString RoomNameFor(FName SessionName, const FOnlineSessionSettings& Settings) const;
	FString HostJsonFor(FName SessionName, const FOnlineSessionSettings& Settings, TArray<FString> Reserved) const;
	void SetRoomOpen(const FNamedOnlineSession& Session, bool bOpen) const;
	void OnRooms(const TArray<FAurixNetRoomSummary>& Rooms);
	void FinishSearch(bool bOk);
	void OnCreateJoined(int32 Actor, EAurixNetError Code, const FString& Error, FName SessionName, FString Room);
	void OnSessionJoined(int32 Actor, EAurixNetError Code, const FString& Error, FName SessionName, FString Room);
	void OnMatchJoined(int32 Actor, EAurixNetError Code, const FString& Error, FName SessionName, FString FallbackJoinJson, TSharedRef<FOnlineSessionSearch> Search, FOnStartMatchmakingComplete Done);
	void FailMatchmaking(FName SessionName, const TSharedRef<FOnlineSessionSearch>& Search, const FOnStartMatchmakingComplete& Done);

	FOnlineSubsystemAurix* Subsystem;
	mutable FCriticalSection SessionLock;
	TArray<FNamedOnlineSession> Sessions;
	TSharedPtr<FOnlineSessionSearch> CurrentSearch;
	FDelegateHandle RoomsHandle;
	double SearchStartedAt = 0.0;
	int32 SearchMinSlots = 0;
};

typedef TSharedPtr<FOnlineSessionAurix, ESPMode::ThreadSafe> FOnlineSessionAurixPtr;
