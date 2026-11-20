#include "AurixNetSubsystem.h"
#include "AurixNetLog.h"
#include "aurix_net.h"
#include "Dom/JsonObject.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"
#include "Serialization/JsonWriter.h"
#include "Policies/CondensedJsonPrintPolicy.h"
#include "AurixNetSettings.h"
#include "Async/Async.h"
#include "Engine/Engine.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "UObject/Package.h"

static FString ToJsonString(const TSharedPtr<FJsonObject>& O)
{
	FString Out;
	if (O.IsValid()) { const TSharedRef<TJsonWriter<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>> Wr = TJsonWriterFactory<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>::Create(&Out); FJsonSerializer::Serialize(O.ToSharedRef(), Wr); }
	return Out;
}
static FString ModeName(EAurixNetRoomMode M)
{
	switch (M) { case EAurixNetRoomMode::Sync: return TEXT("sync"); case EAurixNetRoomMode::Lockstep: return TEXT("lockstep"); default: return TEXT("relay"); }
}
static EAurixNetRoomMode ModeFrom(const FString& S)
{
	if (S == TEXT("sync")) return EAurixNetRoomMode::Sync;
	if (S == TEXT("lockstep")) return EAurixNetRoomMode::Lockstep;
	return EAurixNetRoomMode::Relay;
}
/** One control message for the cell: an object that starts with its type. */
class FControlMessage
{
public:
	explicit FControlMessage(const TCHAR* Type)
		: Writer(TJsonWriterFactory<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>::Create(&Text))
	{
		Writer->WriteObjectStart();
		Writer->WriteValue(TEXT("t"), Type);
	}

	template <typename ValueType>
	FControlMessage& Value(const TCHAR* Name, const ValueType& InValue)
	{
		Writer->WriteValue(Name, InValue);
		return *this;
	}

	/** JSON text the caller already holds, as the one value it is; nothing is written for an empty one. */
	FControlMessage& Raw(const TCHAR* Name, const FString& Json)
	{
		if (!Json.IsEmpty())
		{
			if (char* Written = aurix_net_json_value(TCHAR_TO_UTF8(Name), TCHAR_TO_UTF8(*Json)))
			{
				Writer->WriteRawJSONValue(Name, FString(UTF8_TO_TCHAR(Written)));
				aurix_net_string_free(Written);
			}
			else
			{
				bWhole = false;
			}
		}
		return *this;
	}

	template <typename ElementType>
	FControlMessage& Array(const TCHAR* Name, const TArray<ElementType>& Values)
	{
		Writer->WriteArrayStart(Name);
		for (const ElementType& Element : Values)
		{
			Writer->WriteValue(static_cast<int64>(Element));
		}
		Writer->WriteArrayEnd();
		return *this;
	}

	/** A list of strings; nothing is written for an empty one. */
	FControlMessage& Strings(const TCHAR* Name, const TArray<FString>& Values)
	{
		if (Values.Num() > 0)
		{
			Writer->WriteArrayStart(Name);
			for (const FString& Element : Values)
			{
				Writer->WriteValue(Element);
			}
			Writer->WriteArrayEnd();
		}
		return *this;
	}

	/** The message, or no text when a part of it was not one JSON value: the core's error then names the part. */
	FString Finish()
	{
		Writer->WriteObjectEnd();
		Writer->Close();
		return bWhole ? MoveTemp(Text) : FString();
	}

private:
	bool bWhole = true;
	FString Text;
	TSharedRef<TJsonWriter<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>> Writer;
};

static FAurixNetPlayer PlayerFrom(const TSharedPtr<FJsonObject>& O)
{
	FAurixNetPlayer P;
	if (!O.IsValid()) return P;
	P.Actor = (int32)O->GetNumberField(TEXT("actor"));
	O->TryGetStringField(TEXT("player_id"), P.PlayerId);
	O->TryGetStringField(TEXT("name"), P.Name);
	O->TryGetBoolField(TEXT("online"), P.bOnline);
	O->TryGetBoolField(TEXT("spectator"), P.bSpectator);
	const TSharedPtr<FJsonObject>* Props = nullptr;
	if (O->TryGetObjectField(TEXT("props"), Props)) P.PropsJson = ToJsonString(*Props);
	return P;
}
static FAurixNetRoomSummary RoomSummaryFrom(const FJsonObject& Object)
{
	FAurixNetRoomSummary Summary;
	Object.TryGetStringField(TEXT("id"), Summary.RoomId);
	Object.TryGetStringField(TEXT("name"), Summary.Name);
	FString Mode;
	Object.TryGetStringField(TEXT("mode"), Mode);
	Summary.Mode = ModeFrom(Mode);
	Summary.Players = (int32)Object.GetNumberField(TEXT("players"));
	Summary.MaxPlayers = (int32)Object.GetNumberField(TEXT("max_players"));
	Object.TryGetBoolField(TEXT("open"), Summary.bOpen);
	const TSharedPtr<FJsonObject>* Props = nullptr;
	if (Object.TryGetObjectField(TEXT("props"), Props))
	{
		Summary.PropsJson = ToJsonString(*Props);
	}
	return Summary;
}
static TSharedPtr<FJsonObject> Parse(const FString& Json)
{
	TSharedPtr<FJsonObject> O; const TSharedRef<TJsonReader<>> R = TJsonReaderFactory<>::Create(Json);
	FJsonSerializer::Deserialize(R, O); return O;
}

UAurixNetSubsystem* UAurixNetSubsystem::Get(const UObject* WorldContextObject)
{
	const UWorld* World = GEngine ? GEngine->GetWorldFromContextObject(WorldContextObject, EGetWorldErrorMode::LogAndReturnNull) : nullptr;
	const UGameInstance* GameInstance = World ? World->GetGameInstance() : nullptr;
	return GameInstance ? GameInstance->GetSubsystem<UAurixNetSubsystem>() : nullptr;
}

void UAurixNetSubsystem::Deinitialize() { DestroyClient(); Super::Deinitialize(); }

FString UAurixNetSubsystem::GetNativeVersion() { const char* V = aurix_net_version(); return V ? UTF8_TO_TCHAR(V) : FString(); }
FString UAurixNetSubsystem::GetLastError() const { const char* E = aurix_net_last_error(); return E ? UTF8_TO_TCHAR(E) : FString(); }

static_assert((int32)EAurixNetError::InvalidArgument == AURIX_NET_ERR_INVALID_ARGUMENT && (int32)EAurixNetError::Http == AURIX_NET_ERR_HTTP
	&& (int32)EAurixNetError::Connection == AURIX_NET_ERR_CONNECTION && (int32)EAurixNetError::DataPath == AURIX_NET_ERR_DATA_PATH
	&& (int32)EAurixNetError::Disconnected == AURIX_NET_ERR_DISCONNECTED && (int32)EAurixNetError::Timeout == AURIX_NET_ERR_TIMEOUT
	&& (int32)EAurixNetError::Protocol == AURIX_NET_ERR_PROTOCOL && (int32)EAurixNetError::Panic == AURIX_NET_ERR_PANIC
	&& (int32)EAurixNetError::Unauthorized == AURIX_NET_ERR_UNAUTHORIZED
	&& (int32)EAurixNetError::Forbidden == AURIX_NET_ERR_FORBIDDEN && (int32)EAurixNetError::NotFound == AURIX_NET_ERR_NOT_FOUND
	&& (int32)EAurixNetError::Conflict == AURIX_NET_ERR_CONFLICT && (int32)EAurixNetError::InvalidRequest == AURIX_NET_ERR_INVALID_REQUEST
	&& (int32)EAurixNetError::RoomFull == AURIX_NET_ERR_ROOM_FULL && (int32)EAurixNetError::RoomClosed == AURIX_NET_ERR_ROOM_CLOSED
	&& (int32)EAurixNetError::SchemaMismatch == AURIX_NET_ERR_SCHEMA_MISMATCH && (int32)EAurixNetError::QuotaExceeded == AURIX_NET_ERR_QUOTA_EXCEEDED
	&& (int32)EAurixNetError::RateLimited == AURIX_NET_ERR_RATE_LIMITED && (int32)EAurixNetError::Left == AURIX_NET_ERR_LEFT
	&& (int32)EAurixNetError::Server == AURIX_NET_ERR_SERVER,
	"EAurixNetError must match AURIX_NET_ERR_*");

static EAurixNetError LastErrorCode() { return static_cast<EAurixNetError>(aurix_net_last_error_code()); }
/** The native error is kept per thread: an async call fails on a worker thread and reports it on the game thread. */
static void CarryError(EAurixNetError Code, const FString& Text) { aurix_net_set_last_error(static_cast<int32>(Code), TCHAR_TO_UTF8(*Text)); }
EAurixNetError UAurixNetSubsystem::GetLastErrorCode() const { return LastErrorCode(); }

void UAurixNetSubsystem::SetState(EAurixNetState S) { if (State != S) { State = S; OnStateChanged.Broadcast(S); } }

bool UAurixNetSubsystem::Connect(const FString& CellUrl, const FString& Grant, bool bTunnel)
{
	if (bBusy)
	{
		OnError.Broadcast(TEXT("connect"), TEXT("busy"));
		return false;
	}
	DestroyClient();
	SetState(EAurixNetState::Connecting);
	Client = aurix_net_connect(TCHAR_TO_UTF8(*CellUrl), TCHAR_TO_UTF8(*Grant), bTunnel ? 1 : 0);
	if (!Client)
	{
		UE_LOG(LogAurixNet, Error, TEXT("connect failed: %s"), *GetLastError());
		SetState(EAurixNetState::Disconnected);
		OnError.Broadcast(TEXT("connect"), GetLastError());
		return false;
	}
	ReadPlayer();
	SetState(EAurixNetState::Connected);
	return true;
}

int32 UAurixNetSubsystem::JoinJson(const FString& Json)
{
	if (!Client || bBusy) { OnError.Broadcast(TEXT("join"), bBusy ? TEXT("busy") : TEXT("not connected")); return -1; }
	// No text: a part of the join was refused as it was written, and the core's error names it.
	if (Json.IsEmpty()) { OnError.Broadcast(TEXT("join"), GetLastError()); return -1; }
	const int32 Actor = aurix_net_join(Client, TCHAR_TO_UTF8(*Json));
	if (Actor < 0) { UE_LOG(LogAurixNet, Warning, TEXT("join failed: %s"), *GetLastError()); OnError.Broadcast(TEXT("join"), GetLastError()); return -1; }
	FinishJoin();
	return Actor;
}
void UAurixNetSubsystem::FinishJoin()
{
	ReadJoined();
	RoomPropsJson = Room.PropsJson;
	PlayerProps.Reset();
	for (const FAurixNetPlayer& P : Room.Players) PlayerProps.Add(P.Actor, P.PropsJson);
	SetState(EAurixNetState::InRoom);
	OnJoined.Broadcast(Room);
}

FString UAurixNetSubsystem::JoinOrCreateJson(const FString& RoomName, EAurixNetRoomMode Mode, int32 MaxPlayers, const FString& PropsJson, const FString& LobbyPropsJson, bool bVisible, bool bOpen, const TArray<FString>& Reserved)
{
	return FControlMessage(TEXT("join_or_create"))
		.Value(TEXT("name"), RoomName)
		.Value(TEXT("mode"), ModeName(Mode))
		.Value(TEXT("max_players"), static_cast<int64>(FMath::Clamp(MaxPlayers, 1, 65535)))
		.Value(TEXT("visible"), bVisible)
		.Value(TEXT("open"), bOpen)
		.Raw(TEXT("props"), PropsJson)
		.Raw(TEXT("lobby_props"), LobbyPropsJson)
		.Strings(TEXT("reserved"), Reserved)
		.Finish();
}

FString UAurixNetSubsystem::JoinOrCreateJson(const FString& RoomName, EAurixNetRoomMode Mode, int32 MaxPlayers, const FString& PropsJson, const TArray<FString>& LobbyProps, bool bVisible, bool bOpen, const TArray<FString>& Reserved)
{
	return FControlMessage(TEXT("join_or_create"))
		.Value(TEXT("name"), RoomName)
		.Value(TEXT("mode"), ModeName(Mode))
		.Value(TEXT("max_players"), static_cast<int64>(FMath::Clamp(MaxPlayers, 1, 65535)))
		.Value(TEXT("visible"), bVisible)
		.Value(TEXT("open"), bOpen)
		.Raw(TEXT("props"), PropsJson)
		.Strings(TEXT("lobby_props"), LobbyProps)
		.Strings(TEXT("reserved"), Reserved)
		.Finish();
}

FString UAurixNetSubsystem::JoinJsonFor(const FString& RoomName)
{
	return FControlMessage(TEXT("join")).Value(TEXT("name"), RoomName).Finish();
}

FString UAurixNetSubsystem::JoinRandomJson(const FString& FilterJson, const FString& Expr)
{
	FControlMessage Message(TEXT("join_random"));
	Message.Raw(TEXT("filter"), FilterJson);
	if (!Expr.IsEmpty())
	{
		Message.Value(TEXT("expr"), Expr);
	}
	return Message.Finish();
}

bool UAurixNetSubsystem::ConnectWithSettings(const FString& Grant)
{
	const UAurixNetSettings* S = UAurixNetSettings::Get();
	const FString Url = (IsRunningDedicatedServer() && !S->DedicatedNodeUrl.IsEmpty()) ? S->DedicatedNodeUrl : S->CellUrl;
	return Connect(Url, Grant, S->bForceTunnel);
}

void UAurixNetSubsystem::ConnectAsync(const FString& CellUrl, const FString& Grant, bool bTunnel, FAurixNetAsyncResult Done)
{
	if (bBusy) { Done.ExecuteIfBound(-1, EAurixNetError::InvalidArgument, TEXT("busy")); return; }
	DestroyClient();
	bBusy = true;
	const uint32 Serial = ConnectSerial;
	SetState(EAurixNetState::Connecting);
	TWeakObjectPtr<UAurixNetSubsystem> Self(this);
	Async(EAsyncExecution::Thread, [Self, Serial, CellUrl, Grant, bTunnel, Done]()
	{
		AurixNetClient* C = aurix_net_connect(TCHAR_TO_UTF8(*CellUrl), TCHAR_TO_UTF8(*Grant), bTunnel ? 1 : 0);
		const FString Err = C ? FString() : (aurix_net_last_error() ? FString(UTF8_TO_TCHAR(aurix_net_last_error())) : TEXT("connect failed"));
		const EAurixNetError Code = C ? EAurixNetError::None : LastErrorCode();
		AsyncTask(ENamedThreads::GameThread, [Self, Serial, C, Code, Err, Done]()
		{
			if (!Self.IsValid()) { if (C) aurix_net_destroy(C); return; }
			Self->bBusy = false;
			if (Self->ConnectSerial != Serial)
			{
				if (C) aurix_net_destroy(C);
				Done.ExecuteIfBound(-1, EAurixNetError::Disconnected, TEXT("disconnected while connecting"));
				return;
			}
			if (!C) { CarryError(Code, Err); Self->SetState(EAurixNetState::Disconnected); Self->OnError.Broadcast(TEXT("connect"), Err); Done.ExecuteIfBound(-1, Code, Err); return; }
			Self->Client = C;
			Self->ReadPlayer();
			Self->SetState(EAurixNetState::Connected);
			Done.ExecuteIfBound(0, EAurixNetError::None, FString());
		});
	});
}

void UAurixNetSubsystem::JoinAsync(const FString& JoinJson, FAurixNetAsyncResult Done)
{
	if (!Client) { Done.ExecuteIfBound(-1, EAurixNetError::Disconnected, TEXT("not connected")); return; }
	if (bBusy) { Done.ExecuteIfBound(-1, EAurixNetError::InvalidArgument, TEXT("busy")); return; }
	if (JoinJson.IsEmpty()) { Done.ExecuteIfBound(-1, EAurixNetError::InvalidRequest, TEXT("bad message: props, lobby_props or filter is not one JSON value")); return; }
	bBusy = true;
	AurixNetClient* C = Client;
	JoiningClient = C;
	TWeakObjectPtr<UAurixNetSubsystem> Self(this);
	Async(EAsyncExecution::Thread, [Self, C, JoinJson, Done]()
	{
		const int32 Actor = aurix_net_join(C, TCHAR_TO_UTF8(*JoinJson));
		const FString Err = Actor >= 0 ? FString() : (aurix_net_last_error() ? FString(UTF8_TO_TCHAR(aurix_net_last_error())) : TEXT("join failed"));
		const EAurixNetError Code = Actor >= 0 ? EAurixNetError::None : LastErrorCode();
		AsyncTask(ENamedThreads::GameThread, [Self, C, Actor, Code, Err, Done]()
		{
			if (!Self.IsValid())
			{
				aurix_net_destroy(C);
				return;
			}
			Self->bBusy = false;
			Self->JoiningClient = nullptr;
			if (Self->Client != C)
			{
				aurix_net_destroy(C);
				Done.ExecuteIfBound(-1, EAurixNetError::Disconnected, TEXT("disconnected while joining"));
				return;
			}
			if (Actor < 0) { CarryError(Code, Err); Self->OnError.Broadcast(TEXT("join"), Err); Done.ExecuteIfBound(-1, Code, Err); return; }
			Self->FinishJoin();
			Done.ExecuteIfBound(Actor, EAurixNetError::None, FString());
		});
	});
}

#if WITH_EDITOR
void UAurixNetSubsystem::ConnectWithDevGrantAsync(const FString& PlayerName, FAurixNetAsyncResult Done)
{
	const UAurixNetEditorSettings* E = GetDefault<UAurixNetEditorSettings>();
	const UAurixNetSettings* S = UAurixNetSettings::Get();
	if (E->DevApiKey.IsEmpty()) { Done.ExecuteIfBound(-1, EAurixNetError::InvalidArgument, TEXT("the DevApiKey of the Aurix Net (Editor) settings is empty")); return; }
	const UWorld* W = GetWorld();
	const int32 Pie = (W && W->GetOutermost()) ? W->GetOutermost()->GetPIEInstanceID() : -1;
	const FString PlayerId = FString::Printf(TEXT("%s-%d-%s"), *E->DevPlayerPrefix, FMath::Max(0, Pie), *FGuid::NewGuid().ToString(EGuidFormats::Digits).Left(8));
	const FString Name = PlayerName.IsEmpty() ? FString::Printf(TEXT("PIE %d"), FMath::Max(0, Pie)) : PlayerName;
	const FString Url = S->CellUrl, Key = E->DevApiKey; const bool bTunnel = S->bForceTunnel;
	TWeakObjectPtr<UAurixNetSubsystem> Self(this);
	Async(EAsyncExecution::Thread, [Self, Url, Key, PlayerId, Name, bTunnel, Done]()
	{
		char* G = aurix_net_issue_grant(TCHAR_TO_UTF8(*Url), TCHAR_TO_UTF8(*Key), TCHAR_TO_UTF8(*PlayerId), TCHAR_TO_UTF8(*Name));
		const FString Grant = G ? FString(UTF8_TO_TCHAR(G)) : FString();
		if (G) aurix_net_string_free(G);
		const FString Err = G ? FString() : (aurix_net_last_error() ? FString(UTF8_TO_TCHAR(aurix_net_last_error())) : TEXT("grant failed"));
		const EAurixNetError Code = G ? EAurixNetError::None : LastErrorCode();
		AsyncTask(ENamedThreads::GameThread, [Self, Url, Grant, bTunnel, Code, Err, Done]()
		{
			if (!Self.IsValid()) return;
			if (Grant.IsEmpty()) { CarryError(Code, Err); Done.ExecuteIfBound(-1, Code, Err); return; }
			Self->ConnectAsync(Url, Grant, bTunnel, Done);
		});
	});
}
#endif

static const FString EmptyJsonObject(TEXT("{}"));

bool UAurixNetSubsystem::SetRoomProps(const FString& PropsJson, const FString& ExpectedJson)
{
	const FString Message = FControlMessage(TEXT("set_room_props"))
		.Raw(TEXT("props"), PropsJson.IsEmpty() ? EmptyJsonObject : PropsJson)
		.Raw(TEXT("expected"), ExpectedJson)
		.Finish();
	return !Message.IsEmpty() && SendControl(Message);
}

bool UAurixNetSubsystem::SetRoomVisibility(bool bVisible, bool bOpen)
{
	return SendControl(FControlMessage(TEXT("set_room_props"))
		.Raw(TEXT("props"), EmptyJsonObject)
		.Value(TEXT("visible"), bVisible)
		.Value(TEXT("open"), bOpen)
		.Finish());
}

bool UAurixNetSubsystem::SetPlayerProps(const FString& PropsJson, const FString& ExpectedJson)
{
	const FString Message = FControlMessage(TEXT("set_player_props"))
		.Raw(TEXT("props"), PropsJson.IsEmpty() ? EmptyJsonObject : PropsJson)
		.Raw(TEXT("expected"), ExpectedJson)
		.Finish();
	return !Message.IsEmpty() && SendControl(Message);
}

bool UAurixNetSubsystem::SetGroups(const TArray<int32>& Join, const TArray<int32>& Leave)
{
	return SendControl(FControlMessage(TEXT("set_groups")).Array(TEXT("join"), Join).Array(TEXT("leave"), Leave).Finish());
}

bool UAurixNetSubsystem::SendEventEx(uint8 Code, const TArray<uint8>& Payload, EAurixNetTarget Target, const TArray<int32>& Actors, EAurixNetChannel Channel, int32 Group, EAurixNetCache Cache)
{
	if (!Client)
	{
		return false;
	}
	TArray<uint16> ActorIds;
	ActorIds.Reserve(Actors.Num());
	for (const int32 Actor : Actors)
	{
		if (Actor < 0 || Actor > MAX_uint16)
		{
			CarryError(EAurixNetError::InvalidArgument, FString::Printf(TEXT("actor %d is not in 0..65535"), Actor));
			return false;
		}
		ActorIds.Add(static_cast<uint16>(Actor));
	}
	const int32 NativeTarget = ActorIds.Num() ? AURIX_NET_TARGET_ACTORS : static_cast<int32>(Target);
	const int32 NativeGroup = Group > 0 ? Group : -1;
	return aurix_net_send_event_on(Client, static_cast<uint8>(Channel), Code, NativeTarget, ActorIds.GetData(), ActorIds.Num(),
		Payload.GetData(), Payload.Num(), NativeGroup, static_cast<int32>(Cache)) == 0;
}

bool UAurixNetSubsystem::SetView(const TArray<int64>& Viewpoints, const TArray<int64>& Interest)
{
	return SendControl(FControlMessage(TEXT("set_view")).Array(TEXT("viewpoints"), Viewpoints).Array(TEXT("interest"), Interest).Finish());
}

bool UAurixNetSubsystem::SubscribeLobby(bool bSubscribe)
{
	return SendControl(FControlMessage(TEXT("lobby")).Value(TEXT("subscribe"), bSubscribe).Finish());
}

bool UAurixNetSubsystem::RequestStats()
{
	return SendControl(FControlMessage(TEXT("stats")).Finish());
}

int32 UAurixNetSubsystem::Rpc(const FString& Name, const FString& BodyJson)
{
	const int32 Id = NextRpcId++;
	const FString Message = FControlMessage(TEXT("rpc"))
		.Value(TEXT("id"), static_cast<int64>(Id))
		.Value(TEXT("name"), Name)
		.Raw(TEXT("body"), BodyJson.IsEmpty() ? FString(TEXT("null")) : BodyJson)
		.Finish();
	const bool bSent = !Message.IsEmpty() && SendControl(Message);
	return bSent ? Id : -1;
}

bool UAurixNetSubsystem::Kick(int32 Actor, const FString& Reason)
{
	return SendControl(FControlMessage(TEXT("kick")).Value(TEXT("actor"), static_cast<int64>(Actor)).Value(TEXT("reason"), Reason).Finish());
}

bool UAurixNetSubsystem::SetMaster(int32 Actor)
{
	return SendControl(FControlMessage(TEXT("set_master")).Value(TEXT("actor"), static_cast<int64>(Actor)).Finish());
}

int32 UAurixNetSubsystem::JoinOrCreate(const FString& RoomName, EAurixNetRoomMode Mode, int32 MaxPlayers, const FString& PropsJson, const FString& LobbyPropsJson, bool bVisible, bool bOpen)
{
	return JoinJson(JoinOrCreateJson(RoomName, Mode, MaxPlayers, PropsJson, LobbyPropsJson, bVisible, bOpen));
}

int32 UAurixNetSubsystem::JoinOrCreateEx(const FString& RoomName, EAurixNetRoomMode Mode, int32 MaxPlayers, const FString& PropsJson, const TArray<FString>& LobbyProps, bool bVisible, bool bOpen, const TArray<FString>& Reserved)
{
	return JoinJson(JoinOrCreateJson(RoomName, Mode, MaxPlayers, PropsJson, LobbyProps, bVisible, bOpen, Reserved));
}

int32 UAurixNetSubsystem::Join(const FString& RoomName)
{
	return JoinJson(JoinJsonFor(RoomName));
}

int32 UAurixNetSubsystem::JoinRandom(const FString& FilterJson, const FString& Expr)
{
	return JoinJson(JoinRandomJson(FilterJson, Expr));
}

void UAurixNetSubsystem::ReadPlayer()
{
	char* Id = aurix_net_player_id(Client);
	OwnPlayerId = Id ? FString(UTF8_TO_TCHAR(Id)) : FString();
	aurix_net_string_free(Id);
	char* Name = aurix_net_player_name(Client);
	OwnPlayerName = Name ? FString(UTF8_TO_TCHAR(Name)) : FString();
	aurix_net_string_free(Name);
}

void UAurixNetSubsystem::ReadJoined()
{
	Room = FAurixNetRoomInfo();
	char* S = aurix_net_joined_json(Client);
	if (!S) return;
	const TSharedPtr<FJsonObject> O = Parse(UTF8_TO_TCHAR(S));
	aurix_net_string_free(S);
	if (!O.IsValid()) return;
	O->TryGetStringField(TEXT("room"), Room.RoomId);
	O->TryGetStringField(TEXT("name"), Room.Name);
	FString Mode; O->TryGetStringField(TEXT("mode"), Mode); Room.Mode = ModeFrom(Mode);
	Room.Actor = (int32)O->GetNumberField(TEXT("actor"));
	Room.Master = (int32)O->GetNumberField(TEXT("master"));
	double Max = 0; if (O->TryGetNumberField(TEXT("max_players"), Max)) Room.MaxPlayers = (int32)Max;
	const TArray<TSharedPtr<FJsonValue>>* Players = nullptr;
	if (O->TryGetArrayField(TEXT("players"), Players)) { for (const auto& V : *Players) { Room.Players.Add(PlayerFrom(V->AsObject())); } }
	const TSharedPtr<FJsonObject>* Props = nullptr;
	if (O->TryGetObjectField(TEXT("props"), Props)) Room.PropsJson = ToJsonString(*Props);
	double Tick = 0; if (O->TryGetNumberField(TEXT("tick"), Tick)) Room.Tick = (int64)Tick;
	double Tag = 0; if (O->TryGetNumberField(TEXT("next_spawn_tag"), Tag)) Room.NextSpawnTag = (int64)Tag;
}

bool UAurixNetSubsystem::GetSeed(int64& Seed) const
{
	uint64_t Value = 0;
	const bool bHas = Client && aurix_net_joined_seed(Client, &Value) == 0;
	Seed = (int64)Value;
	return bHas;
}

void UAurixNetSubsystem::Leave()
{
	if (!Client) return;
	aurix_net_leave(Client);
	Room = FAurixNetRoomInfo();
	SetState(EAurixNetState::Connected);
}

void UAurixNetSubsystem::Disconnect() { DestroyClient(); SetState(EAurixNetState::Disconnected); }

void UAurixNetSubsystem::DestroyClient()
{
	if (Client && Client != JoiningClient)
	{
		aurix_net_destroy(Client);
	}
	Client = nullptr;
	OwnPlayerId.Reset();
	OwnPlayerName.Reset();
	++ConnectSerial;
	Room = FAurixNetRoomInfo();
}

bool UAurixNetSubsystem::SendEvent(uint8 Code, const TArray<uint8>& Payload, EAurixNetTarget Target, bool bReliable)
{
	if (!Client) return false;
	return aurix_net_send_event(Client, Code, (int32)Target, nullptr, 0, Payload.GetData(), Payload.Num(), bReliable ? 1 : 0) == 0;
}
bool UAurixNetSubsystem::SendEventTo(uint8 Code, TArrayView<const uint8> Payload, TArrayView<const uint16> Actors, bool bReliable)
{
	if (!Client) return false;
	return aurix_net_send_event(Client, Code, AURIX_NET_TARGET_ACTORS, Actors.GetData(), Actors.Num(), Payload.GetData(), Payload.Num(), bReliable ? 1 : 0) == 0;
}
bool UAurixNetSubsystem::RequestRooms(const FString& FilterExpr, int32 Limit)
{
	FControlMessage Message(TEXT("get_rooms"));
	Message.Value(TEXT("limit"), static_cast<int64>(FMath::Clamp(Limit, 1, 500)));
	if (!FilterExpr.IsEmpty())
	{
		Message.Value(TEXT("expr"), FilterExpr);
	}
	return SendControl(Message.Finish());
}
bool UAurixNetSubsystem::SendControl(const FString& Json) { return Client && aurix_net_send_control(Client, TCHAR_TO_UTF8(*Json)) == 0; }
bool UAurixNetSubsystem::SendData(EAurixNetChannel Channel, TArrayView<const uint8> Payload)
{
	return Client && aurix_net_send_data(Client, (uint8)Channel, Payload.GetData(), Payload.Num()) == 0;
}
bool UAurixNetSubsystem::GetLastStats(int32& Rooms, int32& Players) const
{
	Rooms = LastStatsRooms;
	Players = LastStatsPlayers;
	return LastStatsRooms >= 0;
}
void UAurixNetSubsystem::Flush() { if (Client) aurix_net_flush(Client); }

void UAurixNetSubsystem::Tick(float)
{
	if (!Client || bBusy) return;
	AurixNetEvent Ev;
	// Bounded per frame so a burst cannot stall the game thread.
	for (int32 N = 0; N < 4096 && aurix_net_poll(Client, &Ev) == 1; ++N)
	{
		switch (Ev.kind)
		{
		case AURIX_NET_EVENT_DATA:
		{
			OnRawData.Broadcast(Ev.channel, TArrayView<const uint8>(Ev.data, (int32)Ev.data_len));
			FAurixNetEventFrame F;
			if (Room.Mode == EAurixNetRoomMode::Relay && FAurixNetEventFrame::Decode(Ev.data, Ev.data_len, F))
			{
				OnRawEvent.Broadcast(F);
				if (F.Code < 200 && OnEvent.IsBound())
				{
					TArray<uint8> P(F.Payload.GetData(), F.Payload.Num());
					OnEvent.Broadcast(F.Code, (int32)F.From, P);
				}
			}
			break;
		}
		case AURIX_NET_EVENT_CONTROL:
			if (Ev.json) HandleControl(UTF8_TO_TCHAR(Ev.json));
			break;
		case AURIX_NET_EVENT_UNDELIVERED:
		{
			TArray<FAurixNetUndeliveredEvent> Events;
			Events.SetNum((int32)aurix_net_undelivered_count(&Ev));
			for (int32 I = 0; I < Events.Num(); ++I)
			{
				AurixNetUndelivered Sent;
				aurix_net_undelivered_get(&Ev, I, &Sent);
				TArray<uint16> ActorIds;
				ActorIds.SetNumUninitialized((int32)Sent.n_actors);
				aurix_net_undelivered_actors(&Ev, I, ActorIds.GetData(), ActorIds.Num());
				Events[I].Code = Sent.code;
				Events[I].Payload.Append(Sent.data, (int32)Sent.len);
				if (Sent.target != AURIX_NET_TARGET_ACTORS) Events[I].Target = static_cast<EAurixNetTarget>(Sent.target);
				for (const uint16 Actor : ActorIds) Events[I].Actors.Add(Actor);
				Events[I].Group = FMath::Max(Sent.group, 0);
				Events[I].Cache = static_cast<EAurixNetCache>(Sent.cache);
			}
			OnUndelivered.Broadcast(Events);
			break;
		}
		case AURIX_NET_EVENT_CLOSED:
		{
			const FString Reason = Ev.json ? UTF8_TO_TCHAR(Ev.json) : TEXT("closed");
			aurix_net_event_free(&Ev);
			UE_LOG(LogAurixNet, Log, TEXT("connection closed: %s"), *Reason);
			DestroyClient();
			SetState(EAurixNetState::Closed);
			OnClosed.Broadcast(Reason);
			return;
		}
		default: break;
		}
		aurix_net_event_free(&Ev);
	}
}

void UAurixNetSubsystem::HandleControl(const FString& Json)
{
	const TSharedPtr<FJsonObject> O = Parse(Json);
	if (!O.IsValid()) return;
	FString T; O->TryGetStringField(TEXT("t"), T);
	if (T == TEXT("player_joined"))
	{
		const FAurixNetPlayer P = PlayerFrom(O->GetObjectField(TEXT("player")));
		Room.Players.RemoveAll([&](const FAurixNetPlayer& X) { return X.Actor == P.Actor; });
		Room.Players.Add(P);
		PlayerProps.Add(P.Actor, P.PropsJson);
		OnPlayerJoined.Broadcast(P);
	}
	else if (T == TEXT("player_left"))
	{
		const int32 Actor = (int32)O->GetNumberField(TEXT("actor"));
		bool bOffline = false; O->TryGetBoolField(TEXT("offline"), bOffline);
		if (!bOffline) Room.Players.RemoveAll([&](const FAurixNetPlayer& X) { return X.Actor == Actor; });
		else for (FAurixNetPlayer& X : Room.Players) if (X.Actor == Actor) X.bOnline = false;
		OnPlayerLeft.Broadcast(Actor, bOffline);
	}
	else if (T == TEXT("master_changed"))
	{
		Room.Master = (int32)O->GetNumberField(TEXT("actor"));
		OnMasterChanged.Broadcast(Room.Master);
	}
	else if (T == TEXT("notice"))
	{
		FString Text; O->TryGetStringField(TEXT("text"), Text);
		OnNotice.Broadcast(Text);
	}
	else if (T == TEXT("error"))
	{
		FString Code, Msg; O->TryGetStringField(TEXT("code"), Code); O->TryGetStringField(TEXT("message"), Msg);
		UE_LOG(LogAurixNet, Warning, TEXT("server error %s: %s"), *Code, *Msg);
		OnError.Broadcast(Code, Msg);
	}
	else if (T == TEXT("rooms") || T == TEXT("lobby_rooms"))
	{
		LastRooms.Reset();
		const TArray<TSharedPtr<FJsonValue>>* Listed = nullptr;
		if (O->TryGetArrayField(TEXT("rooms"), Listed))
		{
			for (const TSharedPtr<FJsonValue>& Value : *Listed)
			{
				if (const TSharedPtr<FJsonObject> RoomObject = Value->AsObject())
				{
					LastRooms.Add(RoomSummaryFrom(*RoomObject));
				}
			}
		}
		OnRoomsNative.Broadcast(LastRooms);
		OnRoomsListed.Broadcast(LastRooms);
	}
	else if (T == TEXT("room_props"))
	{
		const TSharedPtr<FJsonObject>* P = nullptr; if (O->TryGetObjectField(TEXT("props"), P)) RoomPropsJson = ToJsonString(*P);
		bool bV = true, bO = true; O->TryGetBoolField(TEXT("visible"), bV); O->TryGetBoolField(TEXT("open"), bO);
		OnRoomProps.Broadcast(RoomPropsJson, bV, bO);
	}
	else if (T == TEXT("player_props"))
	{
		const int32 Actor = (int32)O->GetNumberField(TEXT("actor"));
		const TSharedPtr<FJsonObject>* P = nullptr; FString Js; if (O->TryGetObjectField(TEXT("props"), P)) Js = ToJsonString(*P);
		PlayerProps.Add(Actor, Js);
		OnPlayerProps.Broadcast(Actor, Js);
	}
	else if (T == TEXT("lobby_update"))
	{
		bool bRemoved = false;
		O->TryGetBoolField(TEXT("removed"), bRemoved);
		const TSharedPtr<FJsonObject>* RoomObject = nullptr;
		if (O->TryGetObjectField(TEXT("room"), RoomObject) && RoomObject->IsValid())
		{
			OnLobbyUpdate.Broadcast(RoomSummaryFrom(**RoomObject), bRemoved);
		}
	}
	else if (T == TEXT("stats"))
	{
		LastStatsRooms = (int32)O->GetNumberField(TEXT("rooms"));
		LastStatsPlayers = (int32)O->GetNumberField(TEXT("players"));
		OnStats.Broadcast(LastStatsRooms, LastStatsPlayers);
	}
	else if (T == TEXT("rpc_result"))
	{
		bool bOk = false;
		O->TryGetBoolField(TEXT("ok"), bOk);
		FString Err;
		O->TryGetStringField(TEXT("error"), Err);
		FString Body;
		const TSharedPtr<FJsonValue> BodyValue = O->TryGetField(TEXT("body"));
		if (BodyValue.IsValid() && !BodyValue->IsNull() && !BodyValue->TryGetString(Body))
		{
			const TSharedRef<TJsonWriter<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>> Writer = TJsonWriterFactory<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>::Create(&Body);
			FJsonSerializer::Serialize(BodyValue.ToSharedRef(), FString(), Writer);
		}
		OnRpcResult.Broadcast((int32)O->GetNumberField(TEXT("id")), bOk, Body, Err);
	}
	else if (T == TEXT("desync"))
	{
		bool bU = false; O->TryGetBoolField(TEXT("untrusted"), bU);
		OnDesync.Broadcast((int64)O->GetNumberField(TEXT("tick")), (int32)O->GetNumberField(TEXT("actor")), (int32)O->GetNumberField(TEXT("strikes")), bU);
	}
	else if (T == TEXT("left"))
	{
		FString Reason, RoomId; O->TryGetStringField(TEXT("reason"), Reason); O->TryGetStringField(TEXT("room"), RoomId);
		// a left for an earlier room can arrive after the next join
		char* Joined = Client ? aurix_net_joined_json(Client) : nullptr;
		if (Joined)
		{
			aurix_net_string_free(Joined);
		}
		else
		{
			Room = FAurixNetRoomInfo();
			SetState(EAurixNetState::Connected);
		}
		OnLeft.Broadcast(Reason, RoomId);
	}
}


void UAurixNetSubsystem::BestRegionAsync(const FString& CatalogUrl, int32 Probes, FAurixNetRegionResult Done)
{
	Async(EAsyncExecution::Thread, [CatalogUrl, Probes, Done]()
	{
		char* J = aurix_net_best_region(TCHAR_TO_UTF8(*CatalogUrl), Probes);
		FString Json, Err;
		if (J) { Json = UTF8_TO_TCHAR(J); aurix_net_string_free(J); }
		else { Err = aurix_net_last_error() ? FString(UTF8_TO_TCHAR(aurix_net_last_error())) : TEXT("best_region failed"); }
		AsyncTask(ENamedThreads::GameThread, [Json, Err, Done]()
		{
			FString Endpoint, Region; int32 Rtt = -1;
			TSharedPtr<FJsonObject> Obj;
			if (!Json.IsEmpty() && FJsonSerializer::Deserialize(TJsonReaderFactory<>::Create(Json), Obj) && Obj.IsValid())
			{
				Obj->TryGetStringField(TEXT("endpoint"), Endpoint);
				Obj->TryGetStringField(TEXT("region"), Region);
				Obj->TryGetNumberField(TEXT("rtt_ms"), Rtt);
			}
			Done.ExecuteIfBound(Endpoint, Region, Rtt, Json, Err);
		});
	});
}
