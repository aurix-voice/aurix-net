#include "AurixNetSubsystem.h"
#include "AurixNetLog.h"
#include "aurix_net.h"
#include "Dom/JsonObject.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"
#include "Serialization/JsonWriter.h"
#include "Policies/CondensedJsonPrintPolicy.h"

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
	if (!Client) { OnError.Broadcast(TEXT("join"), TEXT("not connected")); return -1; }
	const int32 Actor = aurix_net_join(Client, TCHAR_TO_UTF8(*Json));
	if (Actor < 0) { UE_LOG(LogAurixNet, Warning, TEXT("join failed: %s"), *GetLastError()); OnError.Broadcast(TEXT("join"), GetLastError()); return -1; }
	ReadJoined();
	SetState(EAurixNetState::InRoom);
	OnJoined.Broadcast(Room);
	return Actor;
}

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
void UAurixNetSubsystem::Flush() { if (Client) aurix_net_flush(Client); }

void UAurixNetSubsystem::Tick(float)
{
	if (!Client) return;
	AurixNetEvent Ev;
	// Bounded per frame so a burst cannot stall the game thread.
	for (int32 N = 0; N < 4096 && aurix_net_poll(Client, &Ev) == 1; ++N)
	{
		switch (Ev.kind)
		{
		case AURIX_NET_EVENT_DATA:
		{
			FAurixNetEventFrame F;
			if (FAurixNetEventFrame::Decode(Ev.data, Ev.data_len, F))
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
		Room.Master = (int32)O->GetNumberField(TEXT("master"));
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
	else if (T == TEXT("left"))
	{
		Room = FAurixNetRoomInfo();
		SetState(EAurixNetState::Connected);
	}
}
