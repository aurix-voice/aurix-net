#pragma once
#include "CoreMinimal.h"
#include "Subsystems/GameInstanceSubsystem.h"
#include "Tickable.h"
#include "AurixNetTypes.h"
#include "AurixNetSubsystem.generated.h"

struct AurixNetClient;

DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FAurixNetStateChanged, EAurixNetState, State);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FAurixNetJoined, const FAurixNetRoomInfo&, Room);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FAurixNetPlayerJoined, const FAurixNetPlayer&, Player);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixNetPlayerLeft, int32, Actor, bool, bOffline);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FAurixNetMasterChanged, int32, Master);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FAurixNetEvent, uint8, Code, int32, FromActor, const TArray<uint8>&, Payload);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FAurixNetNotice, const FString&, Text);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixNetError, const FString&, Code, const FString&, Message);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FAurixNetClosed, const FString&, Reason);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FAurixNetRoomsListed, const TArray<FAurixNetRoomSummary>&, Rooms);

/** Native hook for the NetDriver: every relay event frame, before Blueprint delegates. */
DECLARE_MULTICAST_DELEGATE_OneParam(FAurixNetRawEvent, const FAurixNetEventFrame&);
DECLARE_MULTICAST_DELEGATE_OneParam(FAurixNetRoomsNative, const TArray<FAurixNetRoomSummary>&);

/**
 * One Aurix Net connection per game instance: connect with a player grant from your backend
 * (or a development grant from the console), join a room, send and receive relay events. Polled
 * once per frame; every callback fires on the game thread.
 */
UCLASS()
class AURIXNET_API UAurixNetSubsystem : public UGameInstanceSubsystem, public FTickableGameObject
{
	GENERATED_BODY()
public:
	virtual void Deinitialize() override;

	/** Connect to a cell (https://eu-central.net.aurix.run) with a grant; bTunnel forces the WebSocket fallback. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	bool Connect(const FString& CellUrl, const FString& Grant, bool bTunnel = false);

	/** Join a room by name, creating it with these settings when absent. Returns our actor id or -1. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	int32 JoinOrCreate(const FString& RoomName, EAurixNetRoomMode Mode = EAurixNetRoomMode::Relay, int32 MaxPlayers = 8, const FString& PropsJson = TEXT(""));

	/** Join an existing room by name. Returns our actor id or -1. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	int32 Join(const FString& RoomName);

	/** Any open, visible room with a free slot (optionally filtered by lobby props JSON). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	int32 JoinRandom(const FString& FilterJson = TEXT(""));

	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	void Leave();

	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	void Disconnect();

	/** Relay event to the room: Code is yours (1..199; 200+ are reserved for the NetDriver). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	bool SendEvent(uint8 Code, const TArray<uint8>& Payload, EAurixNetTarget Target = EAurixNetTarget::Others, bool bReliable = false);

	/** Relay event to specific actors. */
	bool SendEventTo(uint8 Code, TArrayView<const uint8> Payload, TArrayView<const uint16> Actors, bool bReliable);

	/** Ask the cell for open rooms (optional filter expression, see docs/matchmaking.md); answered by OnRoomsListed. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	bool RequestRooms(const FString& FilterExpr = TEXT(""), int32 Limit = 50);

	/** Raw control-plane message (JSON ClientMsg, see docs/api.md). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	bool SendControl(const FString& Json);

	UFUNCTION(BlueprintPure, Category = "Aurix Net") EAurixNetState GetState() const { return State; }
	UFUNCTION(BlueprintPure, Category = "Aurix Net") const FAurixNetRoomInfo& GetRoom() const { return Room; }
	UFUNCTION(BlueprintPure, Category = "Aurix Net") bool IsMaster() const { return Room.Actor >= 0 && Room.Actor == Room.Master; }
	UFUNCTION(BlueprintPure, Category = "Aurix Net") static FString GetNativeVersion();
	UFUNCTION(BlueprintPure, Category = "Aurix Net") FString GetLastError() const;

	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetStateChanged OnStateChanged;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetJoined OnJoined;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetPlayerJoined OnPlayerJoined;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetPlayerLeft OnPlayerLeft;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetMasterChanged OnMasterChanged;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetEvent OnEvent;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetNotice OnNotice;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetError OnError;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetClosed OnClosed;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetRoomsListed OnRoomsListed;

	FAurixNetRawEvent OnRawEvent;
	FAurixNetRoomsNative OnRoomsNative;

	/** Flush coalesced datagrams now (the NetDriver calls it after each send burst). */
	void Flush();
	bool HasClient() const { return Client != nullptr; }

	// FTickableGameObject
	virtual void Tick(float DeltaTime) override;
	virtual bool IsTickable() const override { return Client != nullptr; }
	virtual TStatId GetStatId() const override { RETURN_QUICK_DECLARE_CYCLE_STAT(UAurixNetSubsystem, STATGROUP_Tickables); }
	virtual bool IsTickableWhenPaused() const override { return true; }

private:
	int32 JoinJson(const FString& Json);
	void ReadJoined();
	void HandleControl(const FString& Json);
	void SetState(EAurixNetState S);
	void DestroyClient();

	AurixNetClient* Client = nullptr;
	EAurixNetState State = EAurixNetState::Disconnected;
	FAurixNetRoomInfo Room;
};
