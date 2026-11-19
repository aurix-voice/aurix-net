#pragma once
#include "CoreMinimal.h"
#include "Kismet/BlueprintAsyncActionBase.h"
#include "AurixNetTypes.h"
#include "AurixNetAsyncActions.generated.h"

class UAurixNetSubsystem;

DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FAurixNetAsyncPin, const FAurixNetRoomInfo&, Room, EAurixNetError, Code, const FString&, Error);

/** Latent Blueprint nodes for Aurix Net (connect / rooms). Each completes on the game thread with Success or Failure. */
UCLASS()
class AURIXNET_API UAurixNetAsyncAction : public UBlueprintAsyncActionBase
{
	GENERATED_BODY()
public:
	UPROPERTY(BlueprintAssignable) FAurixNetAsyncPin OnSuccess;
	UPROPERTY(BlueprintAssignable) FAurixNetAsyncPin OnFailure;

	/** Connect to a cell with a grant (empty CellUrl = Project Settings). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Connect to Aurix"))
	static UAurixNetAsyncAction* ConnectToAurix(UObject* WorldContextObject, const FString& Grant, const FString& CellUrl = TEXT(""));

	/** Connect (if needed) and join or create a room. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Connect and Join Room"))
	static UAurixNetAsyncAction* ConnectAndJoinRoom(UObject* WorldContextObject, const FString& Grant, const FString& RoomName, EAurixNetRoomMode Mode = EAurixNetRoomMode::Relay, int32 MaxPlayers = 0, const FString& PropsJson = TEXT(""));

	UFUNCTION(BlueprintCallable, Category = "Aurix Net", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Join or Create Room"))
	static UAurixNetAsyncAction* JoinOrCreateRoom(UObject* WorldContextObject, const FString& RoomName, EAurixNetRoomMode Mode = EAurixNetRoomMode::Relay, int32 MaxPlayers = 0, const FString& PropsJson = TEXT(""), const FString& LobbyPropsJson = TEXT(""), bool bVisible = true, bool bOpen = true);

	UFUNCTION(BlueprintCallable, Category = "Aurix Net", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Join Room"))
	static UAurixNetAsyncAction* JoinRoom(UObject* WorldContextObject, const FString& RoomName);

	/** Any open visible room with a free slot; FilterJson = lobby props that must match, Expr = filter expression. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Join Random Room"))
	static UAurixNetAsyncAction* JoinRandomRoom(UObject* WorldContextObject, const FString& FilterJson = TEXT(""), const FString& Expr = TEXT(""));

#if WITH_EDITOR
	/** Editor / PIE only: mint a development grant (editor settings DevApiKey) and connect. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net", meta = (BlueprintInternalUseOnly = "true", WorldContext = "WorldContextObject", DisplayName = "Connect with Dev Grant (PIE)", DevelopmentOnly))
	static UAurixNetAsyncAction* ConnectWithDevGrant(UObject* WorldContextObject, const FString& PlayerName = TEXT(""));
#endif

	virtual void Activate() override;

private:
	enum class EOp : uint8 { Connect, ConnectAndJoin, Join, DevGrant };
	EOp Op = EOp::Connect;
	FString Grant, CellUrl, JoinJson, PlayerName;
	TWeakObjectPtr<UAurixNetSubsystem> Net;

	static UAurixNetAsyncAction* Make(UObject* WorldContextObject, EOp Op);
	void Fail(EAurixNetError Code, const FString& Error);
	void Succeed();
	void DoJoin();
};
