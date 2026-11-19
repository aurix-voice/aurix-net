#include "OnlineSessionAurix.h"
#include "OnlineSubsystemAurix.h"
#include "AurixNetSubsystem.h"
#include "OnlineSubsystemUtils.h"
#include "Online/OnlineSessionNames.h"
#include "Misc/ScopeLock.h"

FOnlineSessionAurix::~FOnlineSessionAurix()
{
	if (UAurixNetSubsystem* N = Net()) { if (RoomsHandle.IsValid()) N->OnRoomsNative.Remove(RoomsHandle); }
}

UAurixNetSubsystem* FOnlineSessionAurix::Net() const { return Subsystem ? Subsystem->GetNetSubsystem() : nullptr; }

FString FOnlineSessionAurix::RoomNameFor(FName SessionName, const FOnlineSessionSettings& Settings) const
{
	FString Room;
	if (Settings.Get(SETTING_AURIX_ROOM, Room) && !Room.IsEmpty()) return Room;
	return SessionName.ToString();
}

FUniqueNetIdPtr FOnlineSessionAurix::CreateSessionIdFromString(const FString& S) { return S.IsEmpty() ? FUniqueNetIdPtr() : FUniqueNetIdPtr(FUniqueNetIdString::Create(S, AURIX_SUBSYSTEM)); }

FNamedOnlineSession* FOnlineSessionAurix::GetNamedSession(FName SessionName)
{
	FScopeLock L(&SessionLock);
	for (FNamedOnlineSession& S : Sessions) if (S.SessionName == SessionName) return &S;
	return nullptr;
}
void FOnlineSessionAurix::RemoveNamedSession(FName SessionName)
{
	FScopeLock L(&SessionLock);
	Sessions.RemoveAll([&](const FNamedOnlineSession& S) { return S.SessionName == SessionName; });
}
FNamedOnlineSession* FOnlineSessionAurix::AddNamedSession(FName SessionName, const FOnlineSessionSettings& Settings)
{
	FScopeLock L(&SessionLock);
	return &Sessions.Emplace_GetRef(SessionName, Settings);
}
FNamedOnlineSession* FOnlineSessionAurix::AddNamedSession(FName SessionName, const FOnlineSession& Session)
{
	FScopeLock L(&SessionLock);
	return &Sessions.Emplace_GetRef(SessionName, Session);
}
bool FOnlineSessionAurix::HasPresenceSession()
{
	FScopeLock L(&SessionLock);
	for (const FNamedOnlineSession& S : Sessions) if (S.SessionSettings.bUsesPresence) return true;
	return false;
}
EOnlineSessionState::Type FOnlineSessionAurix::GetSessionState(FName SessionName) const
{
	FScopeLock L(&SessionLock);
	for (const FNamedOnlineSession& S : Sessions) if (S.SessionName == SessionName) return S.SessionState;
	return EOnlineSessionState::NoSession;
}

bool FOnlineSessionAurix::CreateSession(int32 HostingPlayerNum, FName SessionName, const FOnlineSessionSettings& NewSettings)
{
	UAurixNetSubsystem* N = Net();
	const TCHAR* Refusal = GetNamedSession(SessionName) ? TEXT("the session already exists")
		: !N || !N->HasClient() ? TEXT("Aurix Net is not connected (call Connect with a grant first)")
		: N->IsBusy() ? TEXT("another connect or join is running") : nullptr;
	if (Refusal)
	{
		UE_LOG(LogOnlineAurix, Warning, TEXT("CreateSession '%s': %s"), *SessionName.ToString(), Refusal);
		TriggerOnCreateSessionCompleteDelegates(SessionName, false);
		return false;
	}
	const FString Room = RoomNameFor(SessionName, NewSettings);
	const int32 MaxPlayers = FMath::Max(1, NewSettings.NumPublicConnections + NewSettings.NumPrivateConnections);
	if (N->GetState() == EAurixNetState::InRoom)
	{
		N->Leave();
	}
	FNamedOnlineSession* Session = AddNamedSession(SessionName, NewSettings);
	Session->SessionState = EOnlineSessionState::Creating;
	Session->bHosting = true;
	Session->HostingPlayerNum = HostingPlayerNum;
	Session->NumOpenPublicConnections = NewSettings.NumPublicConnections;
	Session->NumOpenPrivateConnections = NewSettings.NumPrivateConnections;
	if (const IOnlineIdentityPtr Identity = Subsystem->GetIdentityInterface())
	{
		Session->OwningUserId = Identity->GetUniquePlayerId(HostingPlayerNum);
		Session->OwningUserName = Identity->GetPlayerNickname(HostingPlayerNum);
		Session->LocalOwnerId = Session->OwningUserId;
	}
	N->JoinAsync(UAurixNetSubsystem::JoinOrCreateJson(Room, EAurixNetRoomMode::Relay, MaxPlayers, FString()),
		FAurixNetAsyncResult::CreateThreadSafeSP(this, &FOnlineSessionAurix::OnCreateJoined, SessionName, Room));
	return true;
}

void FOnlineSessionAurix::OnCreateJoined(int32 Actor, EAurixNetError Code, const FString& Error, FName SessionName, FString Room)
{
	UAurixNetSubsystem* N = Net();
	FNamedOnlineSession* Session = GetNamedSession(SessionName);
	const bool bHosting = Actor >= 0 && N && N->IsMaster();
	if (!Session || !bHosting)
	{
		if (Actor >= 0 && N)
		{
			N->Leave();
		}
		if (Session)
		{
			UE_LOG(LogOnlineAurix, Warning, TEXT("CreateSession: room '%s' %s"), *Room, Actor < 0 ? *Error : TEXT("already has a host"));
			RemoveNamedSession(SessionName);
			TriggerOnCreateSessionCompleteDelegates(SessionName, false);
		}
		return;
	}
	Session->SessionState = EOnlineSessionState::Pending;
	Session->SessionInfo = MakeShared<FOnlineSessionInfoAurix>(Room, N->GetRoom().RoomId);
	TriggerOnCreateSessionCompleteDelegates(SessionName, true);
}

bool FOnlineSessionAurix::CreateSession(const FUniqueNetId&, FName SessionName, const FOnlineSessionSettings& NewSettings) { return CreateSession(0, SessionName, NewSettings); }

bool FOnlineSessionAurix::StartSession(FName SessionName)
{
	FNamedOnlineSession* S = GetNamedSession(SessionName);
	const bool bOk = S && (S->SessionState == EOnlineSessionState::Pending || S->SessionState == EOnlineSessionState::Ended);
	if (bOk) S->SessionState = EOnlineSessionState::InProgress;
	TriggerOnStartSessionCompleteDelegates(SessionName, bOk);
	return bOk;
}
bool FOnlineSessionAurix::UpdateSession(FName SessionName, FOnlineSessionSettings& Updated, bool)
{
	FNamedOnlineSession* S = GetNamedSession(SessionName);
	if (S) S->SessionSettings = Updated;
	TriggerOnUpdateSessionCompleteDelegates(SessionName, S != nullptr);
	return S != nullptr;
}
bool FOnlineSessionAurix::EndSession(FName SessionName)
{
	FNamedOnlineSession* S = GetNamedSession(SessionName);
	const bool bOk = S && S->SessionState == EOnlineSessionState::InProgress;
	if (bOk) S->SessionState = EOnlineSessionState::Ended;
	TriggerOnEndSessionCompleteDelegates(SessionName, bOk);
	return bOk;
}
bool FOnlineSessionAurix::DestroySession(FName SessionName, const FOnDestroySessionCompleteDelegate& Done)
{
	FNamedOnlineSession* S = GetNamedSession(SessionName);
	if (!S) { Done.ExecuteIfBound(SessionName, false); TriggerOnDestroySessionCompleteDelegates(SessionName, false); return false; }
	if (UAurixNetSubsystem* N = Net()) { if (N->GetState() == EAurixNetState::InRoom) N->Leave(); }
	RemoveNamedSession(SessionName);
	Done.ExecuteIfBound(SessionName, true);
	TriggerOnDestroySessionCompleteDelegates(SessionName, true);
	return true;
}
bool FOnlineSessionAurix::IsPlayerInSession(FName SessionName, const FUniqueNetId& UniqueId)
{
	FNamedOnlineSession* S = GetNamedSession(SessionName);
	if (!S) return false;
	for (const FUniqueNetIdRef& P : S->RegisteredPlayers) if (*P == UniqueId) return true;
	return false;
}

bool FOnlineSessionAurix::StartMatchmaking(const TArray<FUniqueNetIdRef>& LocalPlayers, FName SessionName, const FOnlineSessionSettings& NewSettings, TSharedRef<FOnlineSessionSearch>& Search)
{
	TArray<FSessionMatchmakingUser> Users;
	for (const FUniqueNetIdRef& Id : LocalPlayers) Users.Add(FSessionMatchmakingUser{ Id, {} });
	if (StartMatchmaking(Users, SessionName, NewSettings, Search, FOnStartMatchmakingComplete()))
	{
		return true;
	}
	TriggerOnMatchmakingCompleteDelegates(SessionName, false);
	return false;
}
bool FOnlineSessionAurix::StartMatchmaking(const TArray<FSessionMatchmakingUser>&, FName SessionName, const FOnlineSessionSettings& NewSettings, TSharedRef<FOnlineSessionSearch>& Search, const FOnStartMatchmakingComplete& Done)
{
	UAurixNetSubsystem* N = Net();
	if (GetNamedSession(SessionName) || !N || !N->HasClient() || N->IsBusy())
	{
		Done.ExecuteIfBound(SessionName, FOnlineError(false), {});
		return false;
	}
	if (N->GetState() == EAurixNetState::InRoom)
	{
		N->Leave();
	}
	FNamedOnlineSession* Session = AddNamedSession(SessionName, NewSettings);
	Session->SessionState = EOnlineSessionState::Creating;
	Search->SearchState = EOnlineAsyncTaskState::InProgress;
	// Any open relay room with a free slot; when there is none, host one.
	const FString HostJson = UAurixNetSubsystem::JoinOrCreateJson(RoomNameFor(SessionName, NewSettings), EAurixNetRoomMode::Relay, FMath::Max(1, NewSettings.NumPublicConnections), FString());
	N->JoinAsync(UAurixNetSubsystem::JoinRandomJson(FString()),
		FAurixNetAsyncResult::CreateThreadSafeSP(this, &FOnlineSessionAurix::OnMatchJoined, SessionName, HostJson, Search, Done));
	return true;
}

void FOnlineSessionAurix::OnMatchJoined(int32 Actor, EAurixNetError Code, const FString& Error, FName SessionName, FString FallbackJoinJson, TSharedRef<FOnlineSessionSearch> Search, FOnStartMatchmakingComplete Done)
{
	UAurixNetSubsystem* N = Net();
	FNamedOnlineSession* Session = GetNamedSession(SessionName);
	if (!Session || !N)
	{
		if (Actor >= 0 && N)
		{
			N->Leave();
		}
		FailMatchmaking(SessionName, Search, Done);
		return;
	}
	if (Actor < 0)
	{
		if (FallbackJoinJson.IsEmpty())
		{
			UE_LOG(LogOnlineAurix, Warning, TEXT("StartMatchmaking: %s"), *Error);
			FailMatchmaking(SessionName, Search, Done);
			return;
		}
		N->JoinAsync(FallbackJoinJson, FAurixNetAsyncResult::CreateThreadSafeSP(this, &FOnlineSessionAurix::OnMatchJoined, SessionName, FString(), Search, Done));
		return;
	}
	Session->SessionState = EOnlineSessionState::Pending;
	Session->bHosting = N->IsMaster();
	Session->SessionInfo = MakeShared<FOnlineSessionInfoAurix>(N->GetRoom().Name, N->GetRoom().RoomId);
	Search->SearchState = EOnlineAsyncTaskState::Done;
	Done.ExecuteIfBound(SessionName, FOnlineError(true), {});
	TriggerOnMatchmakingCompleteDelegates(SessionName, true);
}

void FOnlineSessionAurix::FailMatchmaking(FName SessionName, const TSharedRef<FOnlineSessionSearch>& Search, const FOnStartMatchmakingComplete& Done)
{
	RemoveNamedSession(SessionName);
	Search->SearchState = EOnlineAsyncTaskState::Failed;
	Done.ExecuteIfBound(SessionName, FOnlineError(false), {});
	TriggerOnMatchmakingCompleteDelegates(SessionName, false);
}

bool FOnlineSessionAurix::CancelMatchmaking(int32, FName SessionName) { TriggerOnCancelMatchmakingCompleteDelegates(SessionName, true); return true; }
bool FOnlineSessionAurix::CancelMatchmaking(const FUniqueNetId&, FName SessionName) { TriggerOnCancelMatchmakingCompleteDelegates(SessionName, true); return true; }

bool FOnlineSessionAurix::FindSessions(int32, const TSharedRef<FOnlineSessionSearch>& Search)
{
	UAurixNetSubsystem* N = Net();
	if (!N || !N->HasClient() || CurrentSearch.IsValid()) { Search->SearchState = EOnlineAsyncTaskState::Failed; TriggerOnFindSessionsCompleteDelegates(false); return false; }
	CurrentSearch = Search;
	Search->SearchState = EOnlineAsyncTaskState::InProgress;
	Search->SearchResults.Reset();
	if (!RoomsHandle.IsValid()) RoomsHandle = N->OnRoomsNative.AddRaw(this, &FOnlineSessionAurix::OnRooms);
	SearchStartedAt = FPlatformTime::Seconds();
	if (!N->RequestRooms(FString(), FMath::Clamp(Search->MaxSearchResults, 1, 500))) { FinishSearch(false); return false; }
	return true;
}
bool FOnlineSessionAurix::FindSessions(const FUniqueNetId&, const TSharedRef<FOnlineSessionSearch>& Search) { return FindSessions(0, Search); }

void FOnlineSessionAurix::OnRooms(const TArray<FAurixNetRoomSummary>& Rooms)
{
	if (!CurrentSearch.IsValid()) return;
	for (const FAurixNetRoomSummary& R : Rooms)
	{
		if (R.Mode != EAurixNetRoomMode::Relay) continue;
		FOnlineSessionSearchResult& Res = CurrentSearch->SearchResults.Emplace_GetRef();
		Res.Session.SessionInfo = MakeShared<FOnlineSessionInfoAurix>(R.Name, R.RoomId);
		Res.Session.SessionSettings.NumPublicConnections = R.MaxPlayers;
		Res.Session.SessionSettings.bShouldAdvertise = true;
		Res.Session.SessionSettings.bAllowJoinInProgress = R.bOpen;
		Res.Session.SessionSettings.Set(SETTING_AURIX_ROOM, R.Name, EOnlineDataAdvertisementType::ViaOnlineService);
		Res.Session.NumOpenPublicConnections = FMath::Max(0, R.MaxPlayers - R.Players);
		Res.Session.OwningUserName = R.Name;
		Res.PingInMs = 0;
	}
	FinishSearch(true);
}
void FOnlineSessionAurix::FinishSearch(bool bOk)
{
	if (CurrentSearch.IsValid()) CurrentSearch->SearchState = bOk ? EOnlineAsyncTaskState::Done : EOnlineAsyncTaskState::Failed;
	CurrentSearch.Reset();
	TriggerOnFindSessionsCompleteDelegates(bOk);
}
void FOnlineSessionAurix::Tick(float)
{
	if (CurrentSearch.IsValid() && FPlatformTime::Seconds() - SearchStartedAt > 10.0) { UE_LOG(LogOnlineAurix, Warning, TEXT("FindSessions: no room list from the cell in 10 s")); FinishSearch(false); }
}
bool FOnlineSessionAurix::FindSessionById(const FUniqueNetId& User, const FUniqueNetId& SessionId, const FUniqueNetId&, const FOnSingleSessionResultCompleteDelegate& Done)
{
	// Rooms are addressable by name: build the result directly; Join validates against the cell.
	FOnlineSessionSearchResult Res;
	Res.Session.SessionInfo = MakeShared<FOnlineSessionInfoAurix>(SessionId.ToString(), SessionId.ToString());
	Res.Session.SessionSettings.Set(SETTING_AURIX_ROOM, SessionId.ToString(), EOnlineDataAdvertisementType::ViaOnlineService);
	Done.ExecuteIfBound(0, true, Res);
	return true;
}
bool FOnlineSessionAurix::CancelFindSessions() { FinishSearch(false); TriggerOnCancelFindSessionsCompleteDelegates(true); return true; }
bool FOnlineSessionAurix::PingSearchResults(const FOnlineSessionSearchResult&) { return false; }

bool FOnlineSessionAurix::JoinSession(int32 PlayerNum, FName SessionName, const FOnlineSessionSearchResult& Desired)
{
	if (GetNamedSession(SessionName))
	{
		TriggerOnJoinSessionCompleteDelegates(SessionName, EOnJoinSessionCompleteResult::AlreadyInSession);
		return false;
	}
	UAurixNetSubsystem* N = Net();
	FString Room;
	if (Desired.Session.SessionInfo.IsValid())
	{
		Room = StaticCastSharedPtr<FOnlineSessionInfoAurix>(Desired.Session.SessionInfo)->RoomName;
	}
	if (Room.IsEmpty())
	{
		Desired.Session.SessionSettings.Get(SETTING_AURIX_ROOM, Room);
	}
	if (!N || !N->HasClient() || N->IsBusy() || Room.IsEmpty())
	{
		TriggerOnJoinSessionCompleteDelegates(SessionName, EOnJoinSessionCompleteResult::UnknownError);
		return false;
	}
	if (N->GetState() == EAurixNetState::InRoom)
	{
		N->Leave();
	}
	FNamedOnlineSession* Session = AddNamedSession(SessionName, Desired.Session);
	Session->SessionState = EOnlineSessionState::Creating;
	Session->bHosting = false;
	Session->HostingPlayerNum = PlayerNum;
	N->JoinAsync(UAurixNetSubsystem::JoinJsonFor(Room), FAurixNetAsyncResult::CreateThreadSafeSP(this, &FOnlineSessionAurix::OnSessionJoined, SessionName, Room));
	return true;
}

void FOnlineSessionAurix::OnSessionJoined(int32 Actor, EAurixNetError Code, const FString& Error, FName SessionName, FString Room)
{
	UAurixNetSubsystem* N = Net();
	FNamedOnlineSession* Session = GetNamedSession(SessionName);
	if (!Session)
	{
		if (Actor >= 0 && N)
		{
			N->Leave();
		}
		return;
	}
	if (Actor < 0 || !N)
	{
		const EOnJoinSessionCompleteResult::Type Result = Code == EAurixNetError::RoomFull ? EOnJoinSessionCompleteResult::SessionIsFull
			: Code == EAurixNetError::NotFound ? EOnJoinSessionCompleteResult::SessionDoesNotExist
			: EOnJoinSessionCompleteResult::UnknownError;
		RemoveNamedSession(SessionName);
		TriggerOnJoinSessionCompleteDelegates(SessionName, Result);
		return;
	}
	Session->SessionState = EOnlineSessionState::Pending;
	Session->SessionInfo = MakeShared<FOnlineSessionInfoAurix>(Room, N->GetRoom().RoomId);
	TriggerOnJoinSessionCompleteDelegates(SessionName, EOnJoinSessionCompleteResult::Success);
}

bool FOnlineSessionAurix::JoinSession(const FUniqueNetId&, FName SessionName, const FOnlineSessionSearchResult& Desired) { return JoinSession(0, SessionName, Desired); }

bool FOnlineSessionAurix::FindFriendSession(int32 LocalUserNum, const FUniqueNetId&) { TriggerOnFindFriendSessionCompleteDelegates(LocalUserNum, false, {}); return false; }
bool FOnlineSessionAurix::FindFriendSession(const FUniqueNetId&, const FUniqueNetId&) { TriggerOnFindFriendSessionCompleteDelegates(0, false, {}); return false; }
bool FOnlineSessionAurix::FindFriendSession(const FUniqueNetId&, const TArray<FUniqueNetIdRef>&) { TriggerOnFindFriendSessionCompleteDelegates(0, false, {}); return false; }
bool FOnlineSessionAurix::SendSessionInviteToFriend(int32, FName, const FUniqueNetId&) { return false; }
bool FOnlineSessionAurix::SendSessionInviteToFriend(const FUniqueNetId&, FName, const FUniqueNetId&) { return false; }
bool FOnlineSessionAurix::SendSessionInviteToFriends(int32, FName, const TArray<FUniqueNetIdRef>&) { return false; }
bool FOnlineSessionAurix::SendSessionInviteToFriends(const FUniqueNetId&, FName, const TArray<FUniqueNetIdRef>&) { return false; }

bool FOnlineSessionAurix::GetResolvedConnectString(FName SessionName, FString& ConnectInfo, FName)
{
	FNamedOnlineSession* S = GetNamedSession(SessionName);
	if (!S || !S->SessionInfo.IsValid()) return false;
	ConnectInfo = FString::Printf(TEXT("%s.aurix"), *StaticCastSharedPtr<FOnlineSessionInfoAurix>(S->SessionInfo)->RoomName);
	return true;
}
bool FOnlineSessionAurix::GetResolvedConnectString(const FOnlineSessionSearchResult& R, FName, FString& ConnectInfo)
{
	if (!R.Session.SessionInfo.IsValid()) return false;
	ConnectInfo = FString::Printf(TEXT("%s.aurix"), *StaticCastSharedPtr<FOnlineSessionInfoAurix>(R.Session.SessionInfo)->RoomName);
	return true;
}
FOnlineSessionSettings* FOnlineSessionAurix::GetSessionSettings(FName SessionName) { FNamedOnlineSession* S = GetNamedSession(SessionName); return S ? &S->SessionSettings : nullptr; }

bool FOnlineSessionAurix::RegisterPlayer(FName SessionName, const FUniqueNetId& PlayerId, bool bWasInvited) { return RegisterPlayers(SessionName, { PlayerId.AsShared() }, bWasInvited); }
bool FOnlineSessionAurix::RegisterPlayers(FName SessionName, const TArray<FUniqueNetIdRef>& Players, bool)
{
	FNamedOnlineSession* S = GetNamedSession(SessionName);
	if (S) for (const FUniqueNetIdRef& P : Players) { if (!IsPlayerInSession(SessionName, *P)) { S->RegisteredPlayers.Add(P); S->NumOpenPublicConnections = FMath::Max(0, S->NumOpenPublicConnections - 1); } }
	TriggerOnRegisterPlayersCompleteDelegates(SessionName, Players, S != nullptr);
	return S != nullptr;
}
bool FOnlineSessionAurix::UnregisterPlayer(FName SessionName, const FUniqueNetId& PlayerId) { return UnregisterPlayers(SessionName, { PlayerId.AsShared() }); }
bool FOnlineSessionAurix::UnregisterPlayers(FName SessionName, const TArray<FUniqueNetIdRef>& Players)
{
	FNamedOnlineSession* S = GetNamedSession(SessionName);
	if (S) for (const FUniqueNetIdRef& P : Players) { if (S->RegisteredPlayers.RemoveAll([&](const FUniqueNetIdRef& X) { return *X == *P; }) > 0) S->NumOpenPublicConnections++; }
	TriggerOnUnregisterPlayersCompleteDelegates(SessionName, Players, S != nullptr);
	return S != nullptr;
}
void FOnlineSessionAurix::RegisterLocalPlayer(const FUniqueNetId& PlayerId, FName, const FOnRegisterLocalPlayerCompleteDelegate& D) { D.ExecuteIfBound(PlayerId, EOnJoinSessionCompleteResult::Success); }
void FOnlineSessionAurix::UnregisterLocalPlayer(const FUniqueNetId& PlayerId, FName, const FOnUnregisterLocalPlayerCompleteDelegate& D) { D.ExecuteIfBound(PlayerId, true); }
void FOnlineSessionAurix::RemovePlayerFromSession(int32, FName SessionName, const FUniqueNetId& Target) { UnregisterPlayer(SessionName, Target); }
int32 FOnlineSessionAurix::GetNumSessions() { FScopeLock L(&SessionLock); return Sessions.Num(); }
void FOnlineSessionAurix::DumpSessionState()
{
	FScopeLock L(&SessionLock);
	for (const FNamedOnlineSession& S : Sessions) DumpNamedSession(&S);
}
