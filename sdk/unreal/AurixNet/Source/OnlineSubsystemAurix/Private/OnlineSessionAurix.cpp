#include "OnlineSessionAurix.h"
#include "OnlineSubsystemAurix.h"
#include "AurixNetSubsystem.h"
#include "OnlineSubsystemUtils.h"
#include "Online/OnlineSessionNames.h"
#include "Misc/ScopeLock.h"
#include "Dom/JsonObject.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"
#include "Serialization/JsonWriter.h"

namespace
{
	using FCondensedWriter = TJsonWriter<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>;
	using FCondensedWriterFactory = TJsonWriterFactory<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>;

	bool IsAdvertised(FName Key, const FOnlineSessionSetting& Setting)
	{
		return Key != SETTING_AURIX_ROOM && Key != SETTING_AURIX_RESERVED
			&& (Setting.AdvertisementType == EOnlineDataAdvertisementType::ViaOnlineService
				|| Setting.AdvertisementType == EOnlineDataAdvertisementType::ViaOnlineServiceAndPing);
	}

	/** FName ignores case and keeps the first spelling it met, so the prop name is the key in lower case. */
	FString PropName(FName Key) { return Key.ToString().ToLower(); }

	TSharedPtr<FJsonValue> PropValue(const FVariantData& Data)
	{
		switch (Data.GetType())
		{
		case EOnlineKeyValuePairDataType::Int32:
		case EOnlineKeyValuePairDataType::UInt32:
		case EOnlineKeyValuePairDataType::Int64:
		case EOnlineKeyValuePairDataType::UInt64:
			return MakeShared<FJsonValueNumberString>(Data.ToString());
		case EOnlineKeyValuePairDataType::Float: { float Value = 0.f; Data.GetValue(Value); return MakeShared<FJsonValueNumberString>(FString::SanitizeFloat(Value)); }
		case EOnlineKeyValuePairDataType::Double: { double Value = 0.0; Data.GetValue(Value); return MakeShared<FJsonValueNumberString>(FString::SanitizeFloat(Value)); }
		case EOnlineKeyValuePairDataType::String: { FString Value; Data.GetValue(Value); return MakeShared<FJsonValueString>(Value); }
		case EOnlineKeyValuePairDataType::Bool: { bool bValue = false; Data.GetValue(bValue); return MakeShared<FJsonValueBoolean>(bValue); }
		default: return nullptr;
		}
	}

	/** The advertised settings as room props, and their names as the props the room publishes. */
	void RoomPropsOf(const FOnlineSessionSettings& Settings, FString& PropsJson, FString& LobbyPropsJson)
	{
		const TSharedRef<FJsonObject> Props = MakeShared<FJsonObject>();
		TArray<TSharedPtr<FJsonValue>> Names;
		for (const TPair<FName, FOnlineSessionSetting>& Setting : Settings.Settings)
		{
			if (!IsAdvertised(Setting.Key, Setting.Value)) continue;
			const TSharedPtr<FJsonValue> Value = PropValue(Setting.Value.Data);
			if (!Value.IsValid())
			{
				UE_LOG(LogOnlineAurix, Warning, TEXT("Session setting '%s' has a type a room prop cannot hold"), *Setting.Key.ToString());
				continue;
			}
			const FString Name = PropName(Setting.Key);
			Props->SetField(Name, Value);
			Names.Add(MakeShared<FJsonValueString>(Name));
		}
		PropsJson.Reset();
		LobbyPropsJson.Reset();
		FJsonSerializer::Serialize(Props, FCondensedWriterFactory::Create(&PropsJson));
		FJsonSerializer::Serialize(Names, FCondensedWriterFactory::Create(&LobbyPropsJson));
	}

	/** The props a room publishes, as the settings of a search result. */
	void SettingsFromProps(const FString& PropsJson, FOnlineSessionSettings& Settings)
	{
		TSharedPtr<FJsonObject> Props;
		if (PropsJson.IsEmpty() || !FJsonSerializer::Deserialize(TJsonReaderFactory<>::Create(PropsJson), Props) || !Props.IsValid()) return;
		for (const auto& Prop : Props->Values)
		{
			if (!Prop.Value.IsValid()) continue;
			const FName Key(*FString(Prop.Key));
			switch (Prop.Value->Type)
			{
			case EJson::String: Settings.Set(Key, Prop.Value->AsString(), EOnlineDataAdvertisementType::ViaOnlineService); break;
			case EJson::Boolean: Settings.Set(Key, Prop.Value->AsBool(), EOnlineDataAdvertisementType::ViaOnlineService); break;
			case EJson::Number:
			{
				// the engine's settings hold a whole number as int32 or uint64, and nothing wider that is signed
				const FString Text = Prop.Value->AsString();
				int64 Whole = 0;
				uint64 Large = 0;
				if (LexTryParseString(Whole, *Text) && FString::Printf(TEXT("%lld"), Whole) == Text && Whole >= MIN_int32 && Whole <= MAX_int32) Settings.Set(Key, static_cast<int32>(Whole), EOnlineDataAdvertisementType::ViaOnlineService);
				else if (LexTryParseString(Large, *Text) && FString::Printf(TEXT("%llu"), Large) == Text) Settings.Set(Key, Large, EOnlineDataAdvertisementType::ViaOnlineService);
				else Settings.Set(Key, Prop.Value->AsNumber(), EOnlineDataAdvertisementType::ViaOnlineService);
				break;
			}
			default: break;
			}
		}
	}

	bool IsSearchOption(FName Key)
	{
		return Key == SEARCH_DEDICATED_ONLY || Key == SEARCH_EMPTY_SERVERS_ONLY || Key == SEARCH_NONEMPTY_SERVERS_ONLY
			|| Key == SEARCH_SECURE_SERVERS_ONLY || Key == SEARCH_LOBBIES || Key == SEARCH_KEYWORDS
			|| Key == SEARCH_MINSLOTSAVAILABLE;
	}

	bool IsPropName(const FString& Name)
	{
		if (Name.IsEmpty() || !(FChar::IsAlpha(Name[0]) || Name[0] == TEXT('_'))) return false;
		for (const TCHAR Char : Name) if (Char > 127 || !(FChar::IsAlnum(Char) || Char == TEXT('_') || Char == TEXT('.'))) return false;
		const FString Upper = Name.ToUpper();
		return Upper != TEXT("AND") && Upper != TEXT("OR") && Upper != TEXT("NOT") && Upper != TEXT("TRUE") && Upper != TEXT("FALSE");
	}

	const TCHAR* ExprOperator(EOnlineComparisonOp::Type Op)
	{
		switch (Op)
		{
		case EOnlineComparisonOp::Equals: return TEXT("=");
		case EOnlineComparisonOp::NotEquals: return TEXT("!=");
		case EOnlineComparisonOp::GreaterThan: return TEXT(">");
		case EOnlineComparisonOp::GreaterThanEquals: return TEXT(">=");
		case EOnlineComparisonOp::LessThan: return TEXT("<");
		case EOnlineComparisonOp::LessThanEquals: return TEXT("<=");
		default: return nullptr;
		}
	}

	bool ExprLiteral(const FVariantData& Data, FString& Out)
	{
		switch (Data.GetType())
		{
		case EOnlineKeyValuePairDataType::Int32:
		case EOnlineKeyValuePairDataType::UInt32:
		case EOnlineKeyValuePairDataType::Int64:
		case EOnlineKeyValuePairDataType::UInt64:
			Out = Data.ToString();
			return true;
		case EOnlineKeyValuePairDataType::Float: { float Value = 0.f; Data.GetValue(Value); Out = FString::SanitizeFloat(Value); return FMath::IsFinite(Value); }
		case EOnlineKeyValuePairDataType::Double: { double Value = 0.0; Data.GetValue(Value); Out = FString::SanitizeFloat(Value); return FMath::IsFinite(Value); }
		case EOnlineKeyValuePairDataType::Bool: { bool bValue = false; Data.GetValue(bValue); Out = bValue ? TEXT("true") : TEXT("false"); return true; }
		case EOnlineKeyValuePairDataType::String:
		{
			FString Value;
			Data.GetValue(Value);
			const TCHAR* Quote = !Value.Contains(TEXT("'")) ? TEXT("'") : !Value.Contains(TEXT("\"")) ? TEXT("\"") : nullptr;
			if (!Quote) return false;
			Out = Quote + Value + Quote;
			return true;
		}
		default: return false;
		}
	}

	bool NumberOf(const FVariantData& Data, double& Out)
	{
		switch (Data.GetType())
		{
		case EOnlineKeyValuePairDataType::Int32: { int32 Value = 0; Data.GetValue(Value); Out = Value; return true; }
		case EOnlineKeyValuePairDataType::UInt32: { uint32 Value = 0; Data.GetValue(Value); Out = Value; return true; }
		case EOnlineKeyValuePairDataType::Int64: { int64 Value = 0; Data.GetValue(Value); Out = static_cast<double>(Value); return true; }
		case EOnlineKeyValuePairDataType::UInt64: { uint64 Value = 0; Data.GetValue(Value); Out = static_cast<double>(Value); return true; }
		case EOnlineKeyValuePairDataType::Float: { float Value = 0.f; Data.GetValue(Value); Out = Value; return FMath::IsFinite(Value); }
		case EOnlineKeyValuePairDataType::Double: { Data.GetValue(Out); return FMath::IsFinite(Out); }
		default: return false;
		}
	}

	/** How far a room's value of a Near param is from the value asked for; a room without that number is the farthest. */
	double NearDistance(const FOnlineSessionSettings& Settings, const TPair<FName, double>& Near)
	{
		const FOnlineSessionSetting* Setting = Settings.Settings.Find(Near.Key);
		double Value = 0.0;
		return Setting && NumberOf(Setting->Data, Value) ? FMath::Abs(Value - Near.Value) : TNumericLimits<double>::Max();
	}

	/**
	 * The search params as the filter expression of a room list or a random join; false when the node cannot evaluate one of them.
	 * A Near param filters nothing: it goes to Near, by which the results are ordered.
	 */
	bool ExprOf(const FOnlineSearchSettings& Query, FString& Expr, int32& MinSlots, TArray<TPair<FName, double>>& Near)
	{
		constexpr int32 MaxLength = 256, MaxTerms = 16;
		Expr.Reset();
		Near.Reset();
		MinSlots = 0;
		int32 Terms = 0;
		for (const TPair<FName, FOnlineSessionSearchParam>& Param : Query.SearchParams)
		{
			if (Param.Key == SEARCH_MINSLOTSAVAILABLE)
			{
				Param.Value.Data.GetValue(MinSlots);
			}
			if (IsSearchOption(Param.Key)) continue;
			double Target = 0.0;
			if (Param.Value.ComparisonOp == EOnlineComparisonOp::Near && NumberOf(Param.Value.Data, Target))
			{
				Near.Emplace(Param.Key, Target);
				continue;
			}
			const FString Name = PropName(Param.Key);
			const TCHAR* Operator = ExprOperator(Param.Value.ComparisonOp);
			FString Literal;
			if (!IsPropName(Name) || !Operator || !ExprLiteral(Param.Value.Data, Literal))
			{
				UE_LOG(LogOnlineAurix, Warning, TEXT("Search param '%s' cannot be written as a room filter"), *Param.Key.ToString());
				return false;
			}
			if (Terms++ > 0) Expr += TEXT(" AND ");
			Expr += FString::Printf(TEXT("%s %s %s"), *Name, Operator, *Literal);
		}
		if (Terms > MaxTerms || Expr.Len() > MaxLength)
		{
			UE_LOG(LogOnlineAurix, Warning, TEXT("Search params make a room filter over %d comparisons or %d characters"), MaxTerms, MaxLength);
			return false;
		}
		return true;
	}

	int32 MaxPlayersOf(const FOnlineSessionSettings& Settings) { return FMath::Max(1, Settings.NumPublicConnections + Settings.NumPrivateConnections); }
}

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

FString FOnlineSessionAurix::HostJsonFor(FName SessionName, const FOnlineSessionSettings& Settings, TArray<FString> Reserved) const
{
	FString PropsJson, LobbyPropsJson, Listed;
	RoomPropsOf(Settings, PropsJson, LobbyPropsJson);
	if (Settings.Get(SETTING_AURIX_RESERVED, Listed))
	{
		TArray<FString> Ids;
		Listed.ParseIntoArray(Ids, TEXT(","));
		for (FString& Id : Ids)
		{
			Id.TrimStartAndEndInline();
			if (!Id.IsEmpty()) Reserved.AddUnique(Id);
		}
	}
	return UAurixNetSubsystem::JoinOrCreateJson(RoomNameFor(SessionName, Settings), EAurixNetRoomMode::Relay, MaxPlayersOf(Settings),
		PropsJson, LobbyPropsJson, Settings.bShouldAdvertise, true, Reserved);
}

void FOnlineSessionAurix::SetRoomOpen(const FNamedOnlineSession& Session, bool bOpen) const
{
	UAurixNetSubsystem* N = Net();
	if (Session.bHosting && N && N->GetState() == EAurixNetState::InRoom)
	{
		N->SetRoomVisibility(Session.SessionSettings.bShouldAdvertise, bOpen);
	}
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
	N->JoinAsync(HostJsonFor(SessionName, NewSettings, {}),
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
	if (bOk)
	{
		S->SessionState = EOnlineSessionState::InProgress;
		SetRoomOpen(*S, S->SessionSettings.bAllowJoinInProgress);
	}
	TriggerOnStartSessionCompleteDelegates(SessionName, bOk);
	return bOk;
}
bool FOnlineSessionAurix::UpdateSession(FName SessionName, FOnlineSessionSettings& Updated, bool bShouldRefreshOnlineData)
{
	FNamedOnlineSession* S = GetNamedSession(SessionName);
	if (S) S->SessionSettings = Updated;
	UAurixNetSubsystem* N = Net();
	if (S && S->bHosting && bShouldRefreshOnlineData && N && N->GetState() == EAurixNetState::InRoom)
	{
		FString PropsJson, LobbyPropsJson;
		RoomPropsOf(Updated, PropsJson, LobbyPropsJson);
		N->SetRoomProps(PropsJson);
		SetRoomOpen(*S, S->SessionState != EOnlineSessionState::InProgress || Updated.bAllowJoinInProgress);
	}
	TriggerOnUpdateSessionCompleteDelegates(SessionName, S != nullptr);
	return S != nullptr;
}
bool FOnlineSessionAurix::EndSession(FName SessionName)
{
	FNamedOnlineSession* S = GetNamedSession(SessionName);
	const bool bOk = S && S->SessionState == EOnlineSessionState::InProgress;
	if (bOk)
	{
		S->SessionState = EOnlineSessionState::Ended;
		SetRoomOpen(*S, true);
	}
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
bool FOnlineSessionAurix::StartMatchmaking(const TArray<FSessionMatchmakingUser>& LocalPlayers, FName SessionName, const FOnlineSessionSettings& NewSettings, TSharedRef<FOnlineSessionSearch>& Search, const FOnStartMatchmakingComplete& Done)
{
	UAurixNetSubsystem* N = Net();
	FString Expr;
	int32 MinSlots = 0;
	TArray<TPair<FName, double>> Near;
	if (GetNamedSession(SessionName) || !N || !N->HasClient() || N->IsBusy() || !ExprOf(Search->QuerySettings, Expr, MinSlots, Near))
	{
		Done.ExecuteIfBound(SessionName, FOnlineError(false), {});
		return false;
	}
	TArray<FString> Party;
	for (int32 Index = 1; Index < LocalPlayers.Num(); Index++)
	{
		Party.Add(LocalPlayers[Index].UserId->ToString());
	}
	if (N->GetState() == EAurixNetState::InRoom)
	{
		N->Leave();
	}
	FNamedOnlineSession* Session = AddNamedSession(SessionName, NewSettings);
	Session->SessionState = EOnlineSessionState::Creating;
	Search->SearchState = EOnlineAsyncTaskState::InProgress;
	// Any open room with a free slot that the search params accept; when there is none, host one.
	N->JoinAsync(UAurixNetSubsystem::JoinRandomJson(FString(), Expr),
		FAurixNetAsyncResult::CreateThreadSafeSP(this, &FOnlineSessionAurix::OnMatchJoined, SessionName, HostJsonFor(SessionName, NewSettings, Party), Search, Done));
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
	FString Expr;
	if (!N || !N->HasClient() || CurrentSearch.IsValid() || !ExprOf(Search->QuerySettings, Expr, SearchMinSlots, SearchNear)) { Search->SearchState = EOnlineAsyncTaskState::Failed; TriggerOnFindSessionsCompleteDelegates(false); return false; }
	CurrentSearch = Search;
	Search->SearchState = EOnlineAsyncTaskState::InProgress;
	Search->SearchResults.Reset();
	if (!RoomsHandle.IsValid()) RoomsHandle = N->OnRoomsNative.AddRaw(this, &FOnlineSessionAurix::OnRooms);
	SearchStartedAt = FPlatformTime::Seconds();
	if (!N->RequestRooms(Expr, FMath::Clamp(Search->MaxSearchResults, 1, 500))) { FinishSearch(false); return false; }
	return true;
}
bool FOnlineSessionAurix::FindSessions(const FUniqueNetId&, const TSharedRef<FOnlineSessionSearch>& Search) { return FindSessions(0, Search); }

void FOnlineSessionAurix::OnRooms(const TArray<FAurixNetRoomSummary>& Rooms)
{
	if (!CurrentSearch.IsValid()) return;
	for (const FAurixNetRoomSummary& R : Rooms)
	{
		if (R.Mode != EAurixNetRoomMode::Relay || R.MaxPlayers - R.Players < SearchMinSlots) continue;
		FOnlineSessionSearchResult& Res = CurrentSearch->SearchResults.Emplace_GetRef();
		Res.Session.SessionInfo = MakeShared<FOnlineSessionInfoAurix>(R.Name, R.RoomId);
		Res.Session.SessionSettings.NumPublicConnections = R.MaxPlayers;
		Res.Session.SessionSettings.bShouldAdvertise = true;
		Res.Session.SessionSettings.bAllowJoinInProgress = R.bOpen;
		SettingsFromProps(R.PropsJson, Res.Session.SessionSettings);
		Res.Session.SessionSettings.Set(SETTING_AURIX_ROOM, R.Name, EOnlineDataAdvertisementType::ViaOnlineService);
		Res.Session.NumOpenPublicConnections = FMath::Max(0, R.MaxPlayers - R.Players);
		Res.Session.OwningUserName = R.Name;
		Res.PingInMs = 0;
	}
	CurrentSearch->SearchResults.StableSort([this](const FOnlineSessionSearchResult& A, const FOnlineSessionSearchResult& B)
	{
		for (const TPair<FName, double>& Near : SearchNear)
		{
			const double DistanceA = NearDistance(A.Session.SessionSettings, Near);
			const double DistanceB = NearDistance(B.Session.SessionSettings, Near);
			if (DistanceA != DistanceB) return DistanceA < DistanceB;
		}
		return false;
	});
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
		if (Actor < 0)
		{
			UE_LOG(LogOnlineAurix, Warning, TEXT("JoinSession: room '%s' %s"), *Room, *Error);
		}
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
