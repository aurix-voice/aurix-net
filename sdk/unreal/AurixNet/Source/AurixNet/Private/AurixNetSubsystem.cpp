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
#include "Misc/Base64.h"
#include "Engine/World.h"
#include "UObject/Package.h"

static FString ToJsonString(const TSharedPtr<FJsonObject>& O)
{
	FString Out;
	if (O.IsValid()) { const TSharedRef<TJsonWriter<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>> Wr = TJsonWriterFactory<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>::Create(&Out); FJsonSerializer::Serialize(O.ToSharedRef(), Wr); }
	return Out;
}
static FString IntArray(const TArray<int32>& A) { FString S = TEXT("["); for (int32 I = 0; I < A.Num(); ++I) { if (I) S += TEXT(","); S += FString::FromInt(A[I]); } return S + TEXT("]"); }
static FString Int64Array(const TArray<int64>& A) { FString S = TEXT("["); for (int32 I = 0; I < A.Num(); ++I) { if (I) S += TEXT(","); S += FString::Printf(TEXT("%lld"), A[I]); } return S + TEXT("]"); }

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
static FString Quote(const FString& S)
{
	FString Out = TEXT("\"");
	for (TCHAR C : S) { if (C == '"' || C == '\\') { Out.AppendChar('\\'); } Out.AppendChar(C); }
	return Out + TEXT("\"");
}
static FAurixNetPlayer PlayerFrom(const TSharedPtr<FJsonObject>& O)
{
	FAurixNetPlayer P;
	if (!O.IsValid()) return P;
	P.Actor = (int32)O->GetNumberField(TEXT("actor"));
	O->TryGetStringField(TEXT("player_id"), P.PlayerId);
	O->TryGetStringField(TEXT("name"), P.Name);
	O->TryGetBoolField(TEXT("online"), P.bOnline);
	return P;
}
static TSharedPtr<FJsonObject> Parse(const FString& Json)
{
	TSharedPtr<FJsonObject> O; const TSharedRef<TJsonReader<>> R = TJsonReaderFactory<>::Create(Json);
	FJsonSerializer::Deserialize(R, O); return O;
}

void UAurixNetSubsystem::Deinitialize() { DestroyClient(); Super::Deinitialize(); }

FString UAurixNetSubsystem::GetNativeVersion() { const char* V = aurix_net_version(); return V ? UTF8_TO_TCHAR(V) : FString(); }
FString UAurixNetSubsystem::GetLastError() const { const char* E = aurix_net_last_error(); return E ? UTF8_TO_TCHAR(E) : FString(); }

void UAurixNetSubsystem::SetState(EAurixNetState S) { if (State != S) { State = S; OnStateChanged.Broadcast(S); } }

bool UAurixNetSubsystem::Connect(const FString& CellUrl, const FString& Grant, bool bTunnel)
{
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
	SetState(EAurixNetState::Connected);
	return true;
}

int32 UAurixNetSubsystem::JoinJson(const FString& Json)
{
	if (!Client || bBusy) { OnError.Broadcast(TEXT("join"), bBusy ? TEXT("busy") : TEXT("not connected")); return -1; }
	const int32 Actor = aurix_net_join(Client, TCHAR_TO_UTF8(*Json));
	if (Actor < 0) { UE_LOG(LogAurixNet, Warning, TEXT("join failed: %s"), *GetLastError()); OnError.Broadcast(TEXT("join"), GetLastError()); return -1; }
	FinishJoin(Actor);
	return Actor;
}
void UAurixNetSubsystem::FinishJoin(int32)
{
	ReadJoined();
	RoomPropsJson.Reset(); PlayerProps.Reset();
	SetState(EAurixNetState::InRoom);
	OnJoined.Broadcast(Room);
}

FString UAurixNetSubsystem::JoinOrCreateJson(const FString& RoomName, EAurixNetRoomMode Mode, int32 MaxPlayers, const FString& PropsJson, const FString& LobbyPropsJson, bool bVisible, bool bOpen)
{
	FString J = FString::Printf(TEXT("{\"t\":\"join_or_create\",\"name\":%s,\"mode\":\"%s\",\"max_players\":%d,\"visible\":%s,\"open\":%s"), *Quote(RoomName), *ModeName(Mode), FMath::Clamp(MaxPlayers, 1, 65535), bVisible ? TEXT("true") : TEXT("false"), bOpen ? TEXT("true") : TEXT("false"));
	if (!PropsJson.IsEmpty()) J += TEXT(",\"props\":") + PropsJson;
	if (!LobbyPropsJson.IsEmpty()) J += TEXT(",\"lobby_props\":") + LobbyPropsJson;
	return J + TEXT("}");
}
FString UAurixNetSubsystem::JoinJsonFor(const FString& RoomName) { return FString::Printf(TEXT("{\"t\":\"join\",\"name\":%s}"), *Quote(RoomName)); }
FString UAurixNetSubsystem::JoinRandomJson(const FString& FilterJson, const FString& Expr)
{
	FString J = TEXT("{\"t\":\"join_random\"");
	if (!FilterJson.IsEmpty()) J += TEXT(",\"filter\":") + FilterJson;
	if (!Expr.IsEmpty()) J += TEXT(",\"expr\":") + Quote(Expr);
	return J + TEXT("}");
}

bool UAurixNetSubsystem::ConnectWithSettings(const FString& Grant)
{
	const UAurixNetSettings* S = UAurixNetSettings::Get();
	const FString Url = (IsRunningDedicatedServer() && !S->DedicatedNodeUrl.IsEmpty()) ? S->DedicatedNodeUrl : S->CellUrl;
	return Connect(Url, Grant, S->bForceTunnel);
}

void UAurixNetSubsystem::ConnectAsync(const FString& CellUrl, const FString& Grant, bool bTunnel, FAurixNetAsyncResult Done)
{
	if (bBusy) { Done.ExecuteIfBound(-1, TEXT("busy")); return; }
	DestroyClient();
	bBusy = true;
	SetState(EAurixNetState::Connecting);
	TWeakObjectPtr<UAurixNetSubsystem> Self(this);
	Async(EAsyncExecution::Thread, [Self, CellUrl, Grant, bTunnel, Done]()
	{
		AurixNetClient* C = aurix_net_connect(TCHAR_TO_UTF8(*CellUrl), TCHAR_TO_UTF8(*Grant), bTunnel ? 1 : 0);
		const FString Err = C ? FString() : (aurix_net_last_error() ? FString(UTF8_TO_TCHAR(aurix_net_last_error())) : TEXT("connect failed"));
		AsyncTask(ENamedThreads::GameThread, [Self, C, Err, Done]()
		{
			if (!Self.IsValid()) { if (C) aurix_net_destroy(C); return; }
			Self->bBusy = false;
			if (!C) { Self->SetState(EAurixNetState::Disconnected); Self->OnError.Broadcast(TEXT("connect"), Err); Done.ExecuteIfBound(-1, Err); return; }
			Self->Client = C;
			Self->SetState(EAurixNetState::Connected);
			Done.ExecuteIfBound(0, FString());
		});
	});
}

void UAurixNetSubsystem::JoinAsync(const FString& JoinJson, FAurixNetAsyncResult Done)
{
	if (!Client) { Done.ExecuteIfBound(-1, TEXT("not connected")); return; }
	if (bBusy) { Done.ExecuteIfBound(-1, TEXT("busy")); return; }
	bBusy = true;
	AurixNetClient* C = Client;
	TWeakObjectPtr<UAurixNetSubsystem> Self(this);
	Async(EAsyncExecution::Thread, [Self, C, JoinJson, Done]()
	{
		const int32 Actor = aurix_net_join(C, TCHAR_TO_UTF8(*JoinJson));
		const FString Err = Actor >= 0 ? FString() : (aurix_net_last_error() ? FString(UTF8_TO_TCHAR(aurix_net_last_error())) : TEXT("join failed"));
		AsyncTask(ENamedThreads::GameThread, [Self, C, Actor, Err, Done]()
		{
			if (!Self.IsValid()) return;
			Self->bBusy = false;
			if (Self->Client != C) { Done.ExecuteIfBound(-1, TEXT("client replaced")); return; }
			if (Actor < 0) { Self->OnError.Broadcast(TEXT("join"), Err); Done.ExecuteIfBound(-1, Err); return; }
			Self->FinishJoin(Actor);
			Done.ExecuteIfBound(Actor, FString());
		});
	});
}

#if WITH_EDITOR
void UAurixNetSubsystem::ConnectWithDevGrantAsync(const FString& PlayerName, FAurixNetAsyncResult Done)
{
	const UAurixNetEditorSettings* E = GetDefault<UAurixNetEditorSettings>();
	const UAurixNetSettings* S = UAurixNetSettings::Get();
	if (E->DevApiKey.IsEmpty()) { Done.ExecuteIfBound(-1, TEXT("Aurix Net (Editor) → DevApiKey is empty")); return; }
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
		AsyncTask(ENamedThreads::GameThread, [Self, Url, Grant, bTunnel, Err, Done]()
		{
			if (!Self.IsValid()) return;
			if (Grant.IsEmpty()) { Done.ExecuteIfBound(-1, Err); return; }
			Self->ConnectAsync(Url, Grant, bTunnel, Done);
		});
	});
}
#endif

bool UAurixNetSubsystem::SetRoomProps(const FString& PropsJson, const FString& ExpectedJson)
{
	FString J = TEXT("{\"t\":\"set_room_props\",\"props\":") + (PropsJson.IsEmpty() ? TEXT("{}") : PropsJson);
	if (!ExpectedJson.IsEmpty()) J += TEXT(",\"expected\":") + ExpectedJson;
	return SendControl(J + TEXT("}"));
}
bool UAurixNetSubsystem::SetRoomVisibility(bool bVisible, bool bOpen)
{
	return SendControl(FString::Printf(TEXT("{\"t\":\"set_room_props\",\"props\":{},\"visible\":%s,\"open\":%s}"), bVisible ? TEXT("true") : TEXT("false"), bOpen ? TEXT("true") : TEXT("false")));
}
bool UAurixNetSubsystem::SetPlayerProps(const FString& PropsJson, const FString& ExpectedJson)
{
	FString J = TEXT("{\"t\":\"set_player_props\",\"props\":") + (PropsJson.IsEmpty() ? TEXT("{}") : PropsJson);
	if (!ExpectedJson.IsEmpty()) J += TEXT(",\"expected\":") + ExpectedJson;
	return SendControl(J + TEXT("}"));
}
bool UAurixNetSubsystem::SetGroups(const TArray<int32>& Join, const TArray<int32>& Leave)
{
	return SendControl(FString::Printf(TEXT("{\"t\":\"set_groups\",\"join\":%s,\"leave\":%s}"), *IntArray(Join), *IntArray(Leave)));
}
bool UAurixNetSubsystem::SendEventEx(uint8 Code, const TArray<uint8>& Payload, EAurixNetTarget Target, const TArray<int32>& Actors, EAurixNetChannel Channel, int32 Group, EAurixNetCache Cache)
{
	// Fast path: plain events go as binary frames; group/cache need the control-channel form.
	if (Group <= 0 && Cache == EAurixNetCache::None && Actors.Num() == 0) return SendEvent(Code, Payload, Target, Channel != EAurixNetChannel::Unreliable);
	if (Group <= 0 && Cache == EAurixNetCache::None)
	{
		TArray<uint16> A; for (int32 X : Actors) A.Add((uint16)X);
		return SendEventTo(Code, Payload, A, Channel != EAurixNetChannel::Unreliable);
	}
	const TCHAR* Ch = Channel == EAurixNetChannel::Reliable ? TEXT("reliable") : Channel == EAurixNetChannel::ReliableUnordered ? TEXT("reliable_unordered") : TEXT("unreliable");
	const TCHAR* Ca = Cache == EAurixNetCache::Add ? TEXT("add") : Cache == EAurixNetCache::Remove ? TEXT("remove") : TEXT("none");
	FString Tg = Actors.Num() ? FString::Printf(TEXT("{\"kind\":\"actors\",\"actors\":%s}"), *IntArray(Actors))
		: Target == EAurixNetTarget::All ? TEXT("{\"kind\":\"all\"}") : Target == EAurixNetTarget::Master ? TEXT("{\"kind\":\"master\"}") : TEXT("{\"kind\":\"others\"}");
	FString J = FString::Printf(TEXT("{\"t\":\"event\",\"code\":%d,\"data\":%s,\"target\":%s,\"channel\":\"%s\",\"cache\":\"%s\""), (int32)Code, *Quote(FBase64::Encode(Payload)), *Tg, Ch, Ca);
	if (Group > 0) J += FString::Printf(TEXT(",\"group\":%d"), Group);
	return SendControl(J + TEXT("}"));
}
bool UAurixNetSubsystem::SetView(const TArray<int64>& Viewpoints, const TArray<int64>& Interest)
{
	return SendControl(FString::Printf(TEXT("{\"t\":\"set_view\",\"viewpoints\":%s,\"interest\":%s}"), *Int64Array(Viewpoints), *Int64Array(Interest)));
}
bool UAurixNetSubsystem::SubscribeLobby(bool bSubscribe) { return SendControl(FString::Printf(TEXT("{\"t\":\"lobby\",\"subscribe\":%s}"), bSubscribe ? TEXT("true") : TEXT("false"))); }
bool UAurixNetSubsystem::RequestStats() { return SendControl(TEXT("{\"t\":\"stats\"}")); }
int32 UAurixNetSubsystem::Rpc(const FString& Name, const FString& BodyJson)
{
	const int32 Id = NextRpcId++;
	return SendControl(FString::Printf(TEXT("{\"t\":\"rpc\",\"id\":%d,\"name\":%s,\"body\":%s}"), Id, *Quote(Name), BodyJson.IsEmpty() ? TEXT("null") : *BodyJson)) ? Id : -1;
}
bool UAurixNetSubsystem::Kick(int32 Actor, const FString& Reason) { return SendControl(FString::Printf(TEXT("{\"t\":\"kick\",\"actor\":%d,\"reason\":%s}"), Actor, *Quote(Reason))); }
bool UAurixNetSubsystem::SetMaster(int32 Actor) { return SendControl(FString::Printf(TEXT("{\"t\":\"set_master\",\"actor\":%d}"), Actor)); }

int32 UAurixNetSubsystem::JoinOrCreate(const FString& RoomName, EAurixNetRoomMode Mode, int32 MaxPlayers, const FString& PropsJson)
{
	FString J = FString::Printf(TEXT("{\"t\":\"join_or_create\",\"name\":%s,\"mode\":\"%s\",\"max_players\":%d"), *Quote(RoomName), *ModeName(Mode), FMath::Clamp(MaxPlayers, 1, 65535));
	if (!PropsJson.IsEmpty()) J += TEXT(",\"props\":") + PropsJson;
	return JoinJson(J + TEXT("}"));
}
int32 UAurixNetSubsystem::Join(const FString& RoomName) { return JoinJson(FString::Printf(TEXT("{\"t\":\"join\",\"name\":%s}"), *Quote(RoomName))); }
int32 UAurixNetSubsystem::JoinRandom(const FString& FilterJson)
{
	FString J = TEXT("{\"t\":\"join_random\"");
	if (!FilterJson.IsEmpty()) J += TEXT(",\"filter\":") + FilterJson;
	return JoinJson(J + TEXT("}"));
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
	if (Client) { aurix_net_destroy(Client); Client = nullptr; }
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
	FString J = FString::Printf(TEXT("{\"t\":\"get_rooms\",\"limit\":%d"), FMath::Clamp(Limit, 1, 500));
	if (!FilterExpr.IsEmpty()) J += TEXT(",\"expr\":") + Quote(FilterExpr);
	return SendControl(J + TEXT("}"));
}

bool UAurixNetSubsystem::SendControl(const FString& Json) { return Client && aurix_net_send_control(Client, TCHAR_TO_UTF8(*Json)) == 0; }
bool UAurixNetSubsystem::SendData(EAurixNetChannel Channel, TArrayView<const uint8> Payload)
{
	return Client && aurix_net_send_data(Client, (uint8)Channel, Payload.GetData(), Payload.Num()) == 0;
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
		TArray<FAurixNetRoomSummary> Rooms;
		const TArray<TSharedPtr<FJsonValue>>* Arr = nullptr;
		if (O->TryGetArrayField(TEXT("rooms"), Arr))
		{
			for (const auto& V : *Arr)
			{
				const TSharedPtr<FJsonObject> R = V->AsObject(); if (!R.IsValid()) continue;
				FAurixNetRoomSummary S;
				R->TryGetStringField(TEXT("id"), S.RoomId); R->TryGetStringField(TEXT("name"), S.Name);
				FString M; R->TryGetStringField(TEXT("mode"), M); S.Mode = ModeFrom(M);
				S.Players = (int32)R->GetNumberField(TEXT("players")); S.MaxPlayers = (int32)R->GetNumberField(TEXT("max_players"));
				R->TryGetBoolField(TEXT("open"), S.bOpen);
				const TSharedPtr<FJsonObject>* Props = nullptr;
				if (R->TryGetObjectField(TEXT("props"), Props) && Props->IsValid())
				{
					const TSharedRef<TJsonWriter<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>> Wr = TJsonWriterFactory<TCHAR, TCondensedJsonPrintPolicy<TCHAR>>::Create(&S.PropsJson);
					FJsonSerializer::Serialize(Props->ToSharedRef(), Wr);
				}
				Rooms.Add(MoveTemp(S));
			}
		}
		OnRoomsNative.Broadcast(Rooms);
		OnRoomsListed.Broadcast(Rooms);
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
		const TSharedPtr<FJsonObject>* R = nullptr; bool bRemoved = false; O->TryGetBoolField(TEXT("removed"), bRemoved);
		if (O->TryGetObjectField(TEXT("room"), R) && R->IsValid())
		{
			FAurixNetRoomSummary S;
			(*R)->TryGetStringField(TEXT("id"), S.RoomId); (*R)->TryGetStringField(TEXT("name"), S.Name);
			FString M; (*R)->TryGetStringField(TEXT("mode"), M); S.Mode = ModeFrom(M);
			S.Players = (int32)(*R)->GetNumberField(TEXT("players")); S.MaxPlayers = (int32)(*R)->GetNumberField(TEXT("max_players"));
			(*R)->TryGetBoolField(TEXT("open"), S.bOpen);
			const TSharedPtr<FJsonObject>* P = nullptr; if ((*R)->TryGetObjectField(TEXT("props"), P)) S.PropsJson = ToJsonString(*P);
			OnLobbyUpdate.Broadcast(S, bRemoved);
		}
	}
	else if (T == TEXT("stats")) { OnStats.Broadcast((int32)O->GetNumberField(TEXT("rooms")), (int32)O->GetNumberField(TEXT("players"))); }
	else if (T == TEXT("rpc_result"))
	{
		bool bOk = false; O->TryGetBoolField(TEXT("ok"), bOk);
		FString Err; O->TryGetStringField(TEXT("error"), Err);
		FString Body; const TSharedPtr<FJsonValue> BV = O->TryGetField(TEXT("body"));
		if (BV.IsValid()) { if (BV->Type == EJson::Object) Body = ToJsonString(BV->AsObject()); else if (BV->Type == EJson::String) Body = BV->AsString(); else if (BV->Type == EJson::Number) Body = FString::SanitizeFloat(BV->AsNumber()); else if (BV->Type == EJson::Boolean) Body = BV->AsBool() ? TEXT("true") : TEXT("false"); }
		OnRpcResult.Broadcast((int32)O->GetNumberField(TEXT("id")), bOk, Body, Err);
	}
	else if (T == TEXT("desync"))
	{
		bool bU = false; O->TryGetBoolField(TEXT("untrusted"), bU);
		OnDesync.Broadcast((int64)O->GetNumberField(TEXT("tick")), (int32)O->GetNumberField(TEXT("actor")), (int32)O->GetNumberField(TEXT("strikes")), bU);
	}
	else if (T == TEXT("left"))
	{
		Room = FAurixNetRoomInfo();
		SetState(EAurixNetState::Connected);
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
