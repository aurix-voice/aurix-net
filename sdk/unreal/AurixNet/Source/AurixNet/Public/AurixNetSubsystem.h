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
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FAurixNetUndelivered, const TArray<FAurixNetUndeliveredEvent>&, Events);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixNetLeft, const FString&, Reason, const FString&, RoomId);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixNetHostMigration, EAurixNetMigration, Phase, bool, bNewHost);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FAurixNetRoomsListed, const TArray<FAurixNetRoomSummary>&, Rooms);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FAurixNetRoomProps, const FString&, PropsJson, bool, bVisible, bool, bOpen);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixNetPlayerProps, int32, Actor, const FString&, PropsJson);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixNetLobbyUpdate, const FAurixNetRoomSummary&, Room, bool, bRemoved);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixNetStats, int32, Rooms, int32, Players);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_FourParams(FAurixNetRpcResult, int32, Id, bool, bOk, const FString&, BodyJson, const FString&, Error);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_FourParams(FAurixNetDesync, int64, Tick, int32, Actor, int32, Strikes, bool, bUntrusted);
/** Result of an async connect/join: Actor >= 0 on success. */
DECLARE_DELEGATE_ThreeParams(FAurixNetAsyncResult, int32 /*Actor*/, EAurixNetError /*Code*/, const FString& /*Error*/);
/** Best region: endpoint URL (empty on error), region id, RTT ms, full JSON (candidates), error. */
DECLARE_DELEGATE_FiveParams(FAurixNetRegionResult, const FString& /*Endpoint*/, const FString& /*Region*/, int32 /*RttMs*/, const FString& /*Json*/, const FString& /*Error*/);

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

	/**
	 * Join a room by name, creating it with these settings when absent. Returns our actor id or -1.
	 * PropsJson is a JSON object of the room's props; LobbyPropsJson is a JSON array of the names of
	 * those props that room listings show and filters match, for example ["map","rank"].
	 */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	int32 JoinOrCreate(const FString& RoomName, EAurixNetRoomMode Mode = EAurixNetRoomMode::Relay, int32 MaxPlayers = 8, const FString& PropsJson = TEXT(""), const FString& LobbyPropsJson = TEXT(""), bool bVisible = true, bool bOpen = true);

	/**
	 * JoinOrCreate with the names of the published props as a list, and seats held for these player
	 * ids (the players of a matchmaking ticket): nobody else gets them.
	 */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	int32 JoinOrCreateEx(const FString& RoomName, EAurixNetRoomMode Mode, int32 MaxPlayers, const FString& PropsJson, const TArray<FString>& LobbyProps, bool bVisible, bool bOpen, const TArray<FString>& Reserved);

	/** Join an existing room by name. Returns our actor id or -1. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	int32 Join(const FString& RoomName);

	/** Any open, visible room with a free slot (optionally filtered by lobby props JSON and a filter expression). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	int32 JoinRandom(const FString& FilterJson = TEXT(""), const FString& Expr = TEXT(""));

	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	void Leave();

	/** Non-blocking variants (the native connect/join wait for the cell): the work runs on a worker thread, Done fires on the game thread. */
	void ConnectAsync(const FString& CellUrl, const FString& Grant, bool bTunnel, FAurixNetAsyncResult Done);
	/** Region list and best-region ping off the game thread: reads /v1/regions from the catalog, probes every live node, hands back the fastest endpoint to use as the cell URL. */
	void BestRegionAsync(const FString& CatalogUrl, int32 Probes, FAurixNetRegionResult Done);
	void JoinAsync(const FString& JoinJson, FAurixNetAsyncResult Done);
	/** Connect with the project settings cell URL and this grant. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net")
	bool ConnectWithSettings(const FString& Grant);
	/** JSON for the join calls (join_or_create / join / join_random), shared with the async nodes. */
	static FString JoinOrCreateJson(const FString& RoomName, EAurixNetRoomMode Mode, int32 MaxPlayers, const FString& PropsJson, const FString& LobbyPropsJson = TEXT(""), bool bVisible = true, bool bOpen = true, const TArray<FString>& Reserved = TArray<FString>());
	static FString JoinOrCreateJson(const FString& RoomName, EAurixNetRoomMode Mode, int32 MaxPlayers, const FString& PropsJson, const TArray<FString>& LobbyProps, bool bVisible, bool bOpen, const TArray<FString>& Reserved);
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
	/** Subscribe to the lobby: OnRoomsListed with the current list, then OnLobbyUpdate per change.
	 *  A room that left the list comes with bRemoved and, of the room, its RoomId alone, the name
	 *  and the rest empty: take it out by RoomId. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Lobby") bool SubscribeLobby(bool bSubscribe);
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Lobby") bool RequestStats();
	/** Call a room module RPC (rooms with a WASM module); answered by OnRpcResult with this id. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Room") int32 Rpc(const FString& Name, const FString& BodyJson);
	/** Master only. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Room") bool Kick(int32 Actor, const FString& Reason);
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Room") bool SetMaster(int32 Actor);
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Room") FString GetRoomPropsJson() const { return RoomPropsJson; }
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Room") FString GetPlayerPropsJson(int32 Actor) const { const FString* P = PlayerProps.Find(Actor); return P ? *P : FString(); }

	/** Relay NetDriver host migration (see Project Settings > Plugins > Aurix Net). */
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Room") FAurixNetHostMigration OnHostMigration;
	/** Set by the NetDriver across the migration reload so the new world knows it is resuming. */
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net|Room") bool bResumingAfterMigration = false;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Room") FAurixNetRoomProps OnRoomProps;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Room") FAurixNetPlayerProps OnPlayerProps;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Lobby") FAurixNetLobbyUpdate OnLobbyUpdate;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Lobby") FAurixNetStats OnStats;
	/** Rooms from the latest answer to RequestRooms or the lobby subscription. */
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Lobby") const TArray<FAurixNetRoomSummary>& GetLastRooms() const { return LastRooms; }
	/** Room and player counts from the latest answer to RequestStats; false before the first one. */
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Lobby") bool GetLastStats(int32& Rooms, int32& Players) const;
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
	/** The player id of the grant this client signed in with: the id the node knows the player by in rooms, held seats and webhooks. Empty until connected. */
	UFUNCTION(BlueprintPure, Category = "Aurix Net") FString GetPlayerId() const { return OwnPlayerId; }
	/** The display name of the grant this client signed in with. Empty until connected. */
	UFUNCTION(BlueprintPure, Category = "Aurix Net") FString GetPlayerName() const { return OwnPlayerName; }
	UFUNCTION(BlueprintPure, Category = "Aurix Net") const FAurixNetRoomInfo& GetRoom() const { return Room; }
	UFUNCTION(BlueprintPure, Category = "Aurix Net") bool IsMaster() const { return Room.Actor >= 0 && Room.Actor == Room.Master; }
	/** The seed of the lockstep room we are in; false in other rooms and outside a room. */
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Lockstep") bool GetSeed(int64& Seed) const;
	UFUNCTION(BlueprintPure, Category = "Aurix Net") static FString GetNativeVersion();
	/** The text of the last failed call on the game thread: a blocking call, or an async one once it has reported its result. */
	UFUNCTION(BlueprintPure, Category = "Aurix Net") FString GetLastError() const;
	/** The reason of that call. */
	UFUNCTION(BlueprintPure, Category = "Aurix Net") EAurixNetError GetLastErrorCode() const;

	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetStateChanged OnStateChanged;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetJoined OnJoined;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetPlayerJoined OnPlayerJoined;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetPlayerLeft OnPlayerLeft;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetMasterChanged OnMasterChanged;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetEvent OnEvent;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetNotice OnNotice;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetError OnError;
	/** The cell says we are out of room RoomId: Reason is "left", "kicked" or the reason a kick gave, "room closed", "session_replaced", or "rehost" (the room moved to another node: connect again with the same grant and join it). */
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetLeft OnLeft;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetClosed OnClosed;
	/** The reliable events the node had not acknowledged when the room was lost, in the order they were sent. Comes before OnClosed and before the OnLeft of a room the node took us out of; LeaveRoom and Disconnect give none. The room may have got any of them. */
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net") FAurixNetUndelivered OnUndelivered;
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
	/** The subsystem of the game instance that owns this object's world, or null. */
	static UAurixNetSubsystem* Get(const UObject* WorldContextObject);

	// FTickableGameObject
	virtual void Tick(float DeltaTime) override;
	virtual ETickableTickType GetTickableTickType() const override { return IsTemplate() ? ETickableTickType::Never : ETickableTickType::Conditional; }
	virtual bool IsTickable() const override { return Client != nullptr; }
	virtual TStatId GetStatId() const override { RETURN_QUICK_DECLARE_CYCLE_STAT(UAurixNetSubsystem, STATGROUP_Tickables); }
	virtual bool IsTickableWhenPaused() const override { return true; }

private:
	int32 JoinJson(const FString& Json);
	void ReadJoined();
	void ReadPlayer();
	void HandleControl(const FString& Json);
	void SetState(EAurixNetState S);
	void DestroyClient();

	AurixNetClient* Client = nullptr;
	/** The client a JoinAsync worker thread is using; it is destroyed only after the worker returns. */
	AurixNetClient* JoiningClient = nullptr;
	/** Bumped whenever the client is dropped, so a ConnectAsync that was running then discards its result. */
	uint32 ConnectSerial = 0;
	bool bBusy = false;
	TArray<FAurixNetRoomSummary> LastRooms;
	int32 LastStatsRooms = -1;
	int32 LastStatsPlayers = -1;
	int32 NextRpcId = 1;
	FString RoomPropsJson;
	FString OwnPlayerId;
	FString OwnPlayerName;
	TMap<int32, FString> PlayerProps;
	void FinishJoin();
	EAurixNetState State = EAurixNetState::Disconnected;
	FAurixNetRoomInfo Room;
};
