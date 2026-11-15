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
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixNetHostMigration, EAurixNetMigration, Phase, bool, bNewHost);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FAurixNetRoomsListed, const TArray<FAurixNetRoomSummary>&, Rooms);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FAurixNetRoomProps, const FString&, PropsJson, bool, bVisible, bool, bOpen);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixNetPlayerProps, int32, Actor, const FString&, PropsJson);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixNetLobbyUpdate, const FAurixNetRoomSummary&, Room, bool, bRemoved);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixNetStats, int32, Rooms, int32, Players);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_FourParams(FAurixNetRpcResult, int32, Id, bool, bOk, const FString&, BodyJson, const FString&, Error);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_FourParams(FAurixNetDesync, int64, Tick, int32, Actor, int32, Strikes, bool, bUntrusted);
/** Result of an async connect/join: Actor >= 0 on success. */
DECLARE_DELEGATE_TwoParams(FAurixNetAsyncResult, int32 /*Actor*/, const FString& /*Error*/);

/** Native hook for the NetDriver: every relay event frame, before Blueprint delegates. */
DECLARE_MULTICAST_DELEGATE_OneParam(FAurixNetRawEvent, const FAurixNetEventFrame&);
/** Every data payload as received (channel, bytes); sync and lockstep components feed it to their decoders. */
DECLARE_MULTICAST_DELEGATE_TwoParams(FAurixNetRawData, uint8, TArrayView<const uint8>);
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

	/** Non-blocking variants (the native connect/join wait for the cell): the work runs on a worker thread, Done fires on the game thread. */
	void ConnectAsync(const FString& CellUrl, const FString& Grant, bool bTunnel, FAurixNetAsyncResult Done);
	void JoinAsync(const FString& JoinJson, FAurixNetAsyncResult Done);
	/** Connect with the project settings cell URL and this grant. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	bool ConnectWithSettings(const FString& Grant);
	/** JSON for the join calls (join_or_create / join / join_random), shared with the async nodes. */
	static FString JoinOrCreateJson(const FString& RoomName, EAurixNetRoomMode Mode, int32 MaxPlayers, const FString& PropsJson, const FString& LobbyPropsJson = TEXT(""), bool bVisible = true, bool bOpen = true);
	static FString JoinJsonFor(const FString& RoomName);
	static FString JoinRandomJson(const FString& FilterJson, const FString& Expr = TEXT(""));
	bool IsBusy() const { return bBusy; }

#if WITH_EDITOR
	/** PIE: mint a development grant with the editor-only DevApiKey (one player per PIE instance) and connect. */
	void ConnectWithDevGrantAsync(const FString& PlayerName, FAurixNetAsyncResult Done);
#endif

	// ---- Realtime features
	/** Room properties (JSON object); Expected = CAS: apply only if these current values match. Master or anyone, per room rules. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Room") bool SetRoomProps(const FString& PropsJson, const FString& ExpectedJson = TEXT(""));
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Room") bool SetRoomVisibility(bool bVisible, bool bOpen);
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Room") bool SetPlayerProps(const FString& PropsJson, const FString& ExpectedJson = TEXT(""));
	/** Interest groups (relay): receive only events sent to groups you joined; 0 = everyone. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Room") bool SetGroups(const TArray<int32>& Join, const TArray<int32>& Leave);
	/** Event with full options: group (0 = none), cache (Add keeps it for late joiners; Remove drops cached events with this code from us). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Room") bool SendEventEx(uint8 Code, const TArray<uint8>& Payload, EAurixNetTarget Target, const TArray<int32>& Actors, EAurixNetChannel Channel, int32 Group, EAurixNetCache Cache);
	/** Sync rooms: steer area-of-interest — extra viewpoints (entity ids) and entities always in view. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool SetView(const TArray<int64>& Viewpoints, const TArray<int64>& Interest);
	/** Subscribe to the lobby: OnRoomsListed with the current list, then OnLobbyUpdate per change. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Lobby") bool SubscribeLobby(bool bSubscribe);
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Lobby") bool RequestStats();
	/** Call a room module RPC (rooms with a WASM module); answered by OnRpcResult with this id. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Room") int32 Rpc(const FString& Name, const FString& BodyJson);
	/** Master only. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Room") bool Kick(int32 Actor, const FString& Reason);
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Room") bool SetMaster(int32 Actor);
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Room") FString GetRoomPropsJson() const { return RoomPropsJson; }
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Room") FString GetPlayerPropsJson(int32 Actor) const { const FString* P = PlayerProps.Find(Actor); return P ? *P : FString(); }

	/** Relay NetDriver host migration (see Project Settings → Host migration). */
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Room") FAurixNetHostMigration OnHostMigration;
	/** Set by the NetDriver across the migration reload so the new world knows it is resuming. */
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net|Room") bool bResumingAfterMigration = false;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Room") FAurixNetRoomProps OnRoomProps;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Room") FAurixNetPlayerProps OnPlayerProps;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Lobby") FAurixNetLobbyUpdate OnLobbyUpdate;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Lobby") FAurixNetStats OnStats;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Room") FAurixNetRpcResult OnRpcResult;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Lockstep") FAurixNetDesync OnDesync;

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
	FAurixNetRawData OnRawData;
	FAurixNetRoomsNative OnRoomsNative;

	/** Flush coalesced datagrams now (the NetDriver calls it after each send burst). */
	void Flush();
	/** Native client handle for the sync/lockstep helpers (null when disconnected). */
	AurixNetClient* NativeClient() const { return Client; }
	/** Send a raw data payload (sync/lockstep rooms). */
	bool SendData(EAurixNetChannel Channel, TArrayView<const uint8> Payload);
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
	bool bBusy = false;
	int32 NextRpcId = 1;
	FString RoomPropsJson;
	TMap<int32, FString> PlayerProps;
	void FinishJoin(int32 Actor);
	EAurixNetState State = EAurixNetState::Disconnected;
	FAurixNetRoomInfo Room;
};
