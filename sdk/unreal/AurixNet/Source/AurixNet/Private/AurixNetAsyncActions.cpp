#include "AurixNetAsyncActions.h"
#include "AurixNetSubsystem.h"
#include "AurixNetSettings.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "Engine/Engine.h"

UAurixNetAsyncAction* UAurixNetAsyncAction::Make(UObject* Ctx, EOp InOp)
{
	UAurixNetAsyncAction* A = NewObject<UAurixNetAsyncAction>();
	A->Op = InOp;
	const UWorld* W = GEngine ? GEngine->GetWorldFromContextObject(Ctx, EGetWorldErrorMode::LogAndReturnNull) : nullptr;
	const UGameInstance* GI = W ? W->GetGameInstance() : nullptr;
	A->Net = GI ? GI->GetSubsystem<UAurixNetSubsystem>() : nullptr;
	A->RegisterWithGameInstance(const_cast<UGameInstance*>(GI));
	return A;
}

UAurixNetAsyncAction* UAurixNetAsyncAction::ConnectToAurix(UObject* Ctx, const FString& Grant, const FString& CellUrl)
{
	UAurixNetAsyncAction* A = Make(Ctx, EOp::Connect); A->Grant = Grant; A->CellUrl = CellUrl; return A;
}
UAurixNetAsyncAction* UAurixNetAsyncAction::ConnectAndJoinRoom(UObject* Ctx, const FString& Grant, const FString& RoomName, EAurixNetRoomMode Mode, int32 MaxPlayers, const FString& PropsJson)
{
	UAurixNetAsyncAction* A = Make(Ctx, EOp::ConnectAndJoin); A->Grant = Grant;
	A->JoinJson = UAurixNetSubsystem::JoinOrCreateJson(RoomName, Mode, MaxPlayers > 0 ? MaxPlayers : UAurixNetSettings::Get()->DefaultMaxPlayers, PropsJson);
	return A;
}
UAurixNetAsyncAction* UAurixNetAsyncAction::JoinOrCreateRoom(UObject* Ctx, const FString& RoomName, EAurixNetRoomMode Mode, int32 MaxPlayers, const FString& PropsJson, const FString& LobbyPropsJson, bool bVisible, bool bOpen)
{
	UAurixNetAsyncAction* A = Make(Ctx, EOp::Join);
	A->JoinJson = UAurixNetSubsystem::JoinOrCreateJson(RoomName, Mode, MaxPlayers > 0 ? MaxPlayers : UAurixNetSettings::Get()->DefaultMaxPlayers, PropsJson, LobbyPropsJson, bVisible, bOpen);
	return A;
}
UAurixNetAsyncAction* UAurixNetAsyncAction::JoinRoom(UObject* Ctx, const FString& RoomName)
{
	UAurixNetAsyncAction* A = Make(Ctx, EOp::Join); A->JoinJson = UAurixNetSubsystem::JoinJsonFor(RoomName); return A;
}
UAurixNetAsyncAction* UAurixNetAsyncAction::JoinRandomRoom(UObject* Ctx, const FString& FilterJson, const FString& Expr)
{
	UAurixNetAsyncAction* A = Make(Ctx, EOp::Join); A->JoinJson = UAurixNetSubsystem::JoinRandomJson(FilterJson, Expr); return A;
}
#if WITH_EDITOR
UAurixNetAsyncAction* UAurixNetAsyncAction::ConnectWithDevGrant(UObject* Ctx, const FString& PlayerName)
{
	UAurixNetAsyncAction* A = Make(Ctx, EOp::DevGrant); A->PlayerName = PlayerName; return A;
}
#endif

void UAurixNetAsyncAction::Fail(const FString& Error) { OnFailure.Broadcast(Net.IsValid() ? Net->GetRoom() : FAurixNetRoomInfo(), Error); SetReadyToDestroy(); }
void UAurixNetAsyncAction::Succeed() { OnSuccess.Broadcast(Net.IsValid() ? Net->GetRoom() : FAurixNetRoomInfo(), FString()); SetReadyToDestroy(); }

void UAurixNetAsyncAction::DoJoin()
{
	Net->JoinAsync(JoinJson, FAurixNetAsyncResult::CreateWeakLambda(this, [this](int32 Actor, const FString& Err) { if (Actor < 0) Fail(Err); else Succeed(); }));
}

void UAurixNetAsyncAction::Activate()
{
	if (!Net.IsValid()) { Fail(TEXT("no game instance / Aurix Net subsystem")); return; }
	const UAurixNetSettings* S = UAurixNetSettings::Get();
	const FString Url = CellUrl.IsEmpty() ? S->CellUrl : CellUrl;
	switch (Op)
	{
	case EOp::Connect:
		Net->ConnectAsync(Url, Grant, S->bForceTunnel, FAurixNetAsyncResult::CreateWeakLambda(this, [this](int32 R, const FString& Err) { if (R < 0) Fail(Err); else Succeed(); }));
		break;
	case EOp::ConnectAndJoin:
		if (Net->HasClient() && Net->GetState() != EAurixNetState::Closed) { if (Net->GetState() == EAurixNetState::InRoom) Net->Leave(); DoJoin(); }
		else Net->ConnectAsync(Url, Grant, S->bForceTunnel, FAurixNetAsyncResult::CreateWeakLambda(this, [this](int32 R, const FString& Err) { if (R < 0) Fail(Err); else DoJoin(); }));
		break;
	case EOp::Join:
		if (!Net->HasClient()) { Fail(TEXT("not connected")); return; }
		if (Net->GetState() == EAurixNetState::InRoom) Net->Leave();
		DoJoin();
		break;
	case EOp::DevGrant:
#if WITH_EDITOR
		Net->ConnectWithDevGrantAsync(PlayerName, FAurixNetAsyncResult::CreateWeakLambda(this, [this](int32 R, const FString& Err) { if (R < 0) Fail(Err); else Succeed(); }));
#else
		Fail(TEXT("development grants are editor-only"));
#endif
		break;
	}
}
