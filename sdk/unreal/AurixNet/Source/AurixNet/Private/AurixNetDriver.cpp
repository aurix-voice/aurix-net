#include "AurixNetDriver.h"
#include "AurixNetConnection.h"
#include "AurixNetSubsystem.h"
#include "AurixNetLog.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "Misc/Parse.h"
#include "AurixNetSettings.h"
#include "Engine/Engine.h"
#include "UObject/Package.h"
#include "PacketHandler.h"
#include "PacketHandlers/StatelessConnectHandlerComponent.h"
#include "Net/Common/Packets/PacketView.h"
#include "SocketTypes.h"

UClass* UAurixNetDriver::ConnectionClass() const
{
	UClass* Configured = NetConnectionClass;
	return Configured && Configured->IsChildOf(UAurixNetConnection::StaticClass()) ? Configured : UAurixNetConnection::StaticClass();
}

UAurixNetSubsystem* UAurixNetDriver::Subsystem() const
{
	const UWorld* W = GetWorld();
	const UGameInstance* GI = W ? W->GetGameInstance() : nullptr;
	// A connecting client's driver has no world until the travel completes; its world context has the game instance.
	if (!GI && GEngine)
	{
		for (const FWorldContext& Ctx : GEngine->GetWorldContexts())
		{
			if (Ctx.ActiveNetDrivers.ContainsByPredicate([this](const FNamedNetDriver& N) { return N.NetDriver == this; }))
			{
				GI = Ctx.OwningGameInstance;
				break;
			}
		}
	}
	return GI ? GI->GetSubsystem<UAurixNetSubsystem>() : nullptr;
}

bool UAurixNetDriver::IsAvailable() const { return true; }

bool UAurixNetDriver::InitBase(bool bInitAsClient, FNetworkNotify* InNotify, const FURL& URL, bool bReuseAddressAndPort, FString& Error)
{
	// These are per-class config and the engine ships values only for its own drivers. Left at zero, a
	// connection times out on its first tick. The values are the IpNetDriver ones; config still overrides.
	if (InitialConnectTimeout <= 0.f) InitialConnectTimeout = 60.f;
	if (ConnectionTimeout <= 0.f) ConnectionTimeout = 60.f;
	if (TimeoutMultiplierForUnoptimizedBuilds <= 0.f) TimeoutMultiplierForUnoptimizedBuilds = 1.f;
	if (RecentlyDisconnectedTrackingTime <= 0) RecentlyDisconnectedTrackingTime = 120;
	if (KeepAliveTime <= 0.f) KeepAliveTime = 0.2f;
	if (RelevantTimeout <= 0.f) RelevantTimeout = 5.f;
	if (SpawnPrioritySeconds <= 0.f) SpawnPrioritySeconds = 1.f;
	if (ServerTravelPause <= 0.f) ServerTravelPause = 4.f;
	if (MaxClientRate <= 0) MaxClientRate = 100000;
	if (MaxInternetClientRate <= 0) MaxInternetClientRate = 100000;
	if (GetNetServerMaxTickRate() <= 0) SetNetServerMaxTickRate(30);
	if (MaxNetTickRate <= 0) MaxNetTickRate = 120;

	if (!Super::InitBase(bInitAsClient, InNotify, URL, bReuseAddressAndPort, Error)) return false;
	UAurixNetSubsystem* S = Subsystem();
	if (!S || !S->HasClient())
	{
		Error = TEXT("Aurix Net: connect the UAurixNetSubsystem to a cell with a grant before travelling to a room");
		return false;
	}
	// The room is the Room= option. A client URL may also carry it as its host, "<room>.aurix": the
	// engine reads a host only from a name with a dot. A listen URL is a local map, so it has no host;
	// without Room= the driver hosts the room this player is already in.
	FString RoomName = URL.GetOption(TEXT("Room="), TEXT(""));
	if (RoomName.IsEmpty() && bInitAsClient) { RoomName = URL.Host; RoomName.RemoveFromEnd(TEXT(".aurix")); }
	if (RoomName.IsEmpty() && S->GetState() != EAurixNetState::InRoom)
	{
		Error = TEXT("Aurix Net: no room to host; add ?Room=<name> to the listen URL or join a room first");
		return false;
	}
	if (S->GetState() != EAurixNetState::InRoom || (!RoomName.IsEmpty() && S->GetRoom().Name != RoomName))
	{
		if (S->GetState() == EAurixNetState::InRoom) S->Leave();
		int32 Max = 16; FParse::Value(*URL.ToString(), TEXT("MaxPlayers="), Max);
		const int32 Actor = bInitAsClient ? S->Join(RoomName) : S->JoinOrCreate(RoomName, EAurixNetRoomMode::Relay, Max);
		if (Actor < 0) { Error = FString::Printf(TEXT("Aurix Net: could not join room '%s': %s"), *RoomName, *S->GetLastError()); return false; }
	}
	if (!bInitAsClient && !S->IsMaster())
	{
		Error = TEXT("Aurix Net: this player is not the room master; only the master can listen");
		return false;
	}
	RawHandle = S->OnRawEvent.AddUObject(this, &UAurixNetDriver::OnRawEvent);
	S->OnPlayerLeft.AddDynamic(this, &UAurixNetDriver::HandlePlayerLeft);
	S->OnMasterChanged.AddDynamic(this, &UAurixNetDriver::HandleMasterChanged);
	S->OnClosed.AddDynamic(this, &UAurixNetDriver::HandleClosed);
	LocalAddr = MakeShared<FInternetAddrAurix>(uint16(S->GetRoom().Actor));
	return true;
}

bool UAurixNetDriver::InitConnect(FNetworkNotify* InNotify, const FURL& ConnectURL, FString& Error)
{
	if (!InitBase(true, InNotify, ConnectURL, false, Error)) return false;
	UAurixNetSubsystem* S = Subsystem();
	ServerActor = uint16(S->GetRoom().Master);
	UAurixNetConnection* C = NewObject<UAurixNetConnection>(GetTransientPackage(), ConnectionClass());
	ServerConnection = C;
	C->InitLocalConnection(this, nullptr, ConnectURL, USOCK_Pending);
	Remotes.Add(ServerActor, C);
	CreateInitialClientChannels();
	if (S->bResumingAfterMigration) { S->bResumingAfterMigration = false; S->OnHostMigration.Broadcast(EAurixNetMigration::Done, false); }
	UE_LOG(LogAurixNet, Log, TEXT("client: joined room '%s' as actor %d, server is actor %u"), *S->GetRoom().Name, S->GetRoom().Actor, ServerActor);
	return true;
}

bool UAurixNetDriver::InitListen(FNetworkNotify* InNotify, FURL& LocalURL, bool bReuseAddressAndPort, FString& Error)
{
	if (!InitBase(false, InNotify, LocalURL, bReuseAddressAndPort, Error)) return false;
	bListening = true;
	InitConnectionlessHandler();
	if (UAurixNetSubsystem* S = Subsystem()) { if (S->bResumingAfterMigration) { S->bResumingAfterMigration = false; S->OnHostMigration.Broadcast(EAurixNetMigration::Done, true); } }
	UE_LOG(LogAurixNet, Log, TEXT("listen: room '%s', master actor %d"), *Subsystem()->GetRoom().Name, Subsystem()->GetRoom().Actor);
	return true;
}

UAurixNetConnection* UAurixNetDriver::AcceptRemote(uint16 Actor, TArray<uint8>& Packet)
{
	// Same order as UIpNetDriver::ProcessConnectionlessPacket: the stateless handshake runs on the
	// connectionless handler, and a connection exists only after the challenge is passed.
	const TSharedPtr<StatelessConnectHandlerComponent> Stateless = StatelessConnectComponent.Pin();
	if (!Notify || !ConnectionlessHandler.IsValid() || !Stateless.IsValid()) return nullptr;
	if (Notify->NotifyAcceptingConnection() != EAcceptConnection::Accept) return nullptr;
	const TSharedRef<FInternetAddrAurix> Addr = MakeShared<FInternetAddrAurix>(Actor);
	FReceivedPacketView View;
	View.DataView = { Packet.GetData(), Packet.Num(), ECountUnits::Bytes };
	View.Address = Addr;
	View.Error = SE_NO_ERROR;
	bool bRestarted = false;
	if (ConnectionlessHandler->IncomingConnectionless(View) != EIncomingResult::Success || !Stateless->HasPassedChallenge(Addr, bRestarted)) return nullptr;

	UAurixNetConnection* C = nullptr;
	if (bRestarted)
	{
		// A client we already know came back under another actor id; its connection moves to that id.
		for (UNetConnection* Cur : ClientConnections)
		{
			const TSharedPtr<StatelessConnectHandlerComponent> Comp = Cur ? Cur->StatelessConnectComponent.Pin() : nullptr;
			if (!Comp.IsValid() || !Stateless->DoesRestartedHandshakeMatch(*Comp)) continue;
			C = Cast<UAurixNetConnection>(Cur);
			if (C)
			{
				Remotes.Remove(C->RemoteActor);
				C->RemoteActor = Actor;
				C->RemoteAddr = Addr;
				Remotes.Add(Actor, C);
				Comp->InitFromConnectionless(Stateless.Get());
			}
			break;
		}
	}
	else
	{
		const UWorld* W = GetWorld();
		C = NewObject<UAurixNetConnection>(GetTransientPackage(), ConnectionClass());
		C->InitRemoteConnection(this, nullptr, W ? W->URL : FURL(), *Addr, USOCK_Open);
		int32 ServerSequence = 0, ClientSequence = 0;
		Stateless->GetChallengeSequence(ServerSequence, ClientSequence);
		C->InitSequence(ClientSequence, ServerSequence);
		if (C->Handler.IsValid()) C->Handler->BeginHandshaking();
		Notify->NotifyAcceptedConnection(C);
		AddClientConnection(C);
		Remotes.Add(Actor, C);
		UE_LOG(LogAurixNet, Log, TEXT("listen: new client connection from actor %u"), Actor);
	}
	Stateless->ResetChallengeData();
	// What the handshake left of the packet belongs to the connection.
	TArray<uint8> Rest(View.DataView.GetData(), View.DataView.NumBytes());
	Packet = MoveTemp(Rest);
	return C;
}

void UAurixNetDriver::OnRawEvent(const FAurixNetEventFrame& F)
{
	if (F.Code != PacketCode) return;
	Inbox.Emplace(F.From, TArray<uint8>(F.Payload.GetData(), F.Payload.Num()));
}

void UAurixNetDriver::TickDispatch(float DeltaTime)
{
	Super::TickDispatch(DeltaTime);
	bSentThisTick = false;
	if (bMigrating) { MigrateIn -= DeltaTime; if (MigrateIn <= 0.f) { bMigrating = false; TravelForMigration(); } return; }
	// The subsystem polled the native client this frame and queued our frames; hand them to the connections.
	TArray<TPair<uint16, TArray<uint8>>> Batch = MoveTemp(Inbox);
	Inbox.Reset();
	for (TPair<uint16, TArray<uint8>>& P : Batch)
	{
		UAurixNetConnection* C = nullptr;
		if (!bListening) { if (P.Key == ServerActor) C = Cast<UAurixNetConnection>(ServerConnection); }
		else if (const TWeakObjectPtr<UAurixNetConnection>* Found = Remotes.Find(P.Key); Found && Found->IsValid()) C = Found->Get();
		else C = AcceptRemote(P.Key, P.Value);
		if (!C || C->GetConnectionState() == USOCK_Closed || P.Value.Num() == 0) continue;
		C->ReceivedRawPacket(P.Value.GetData(), P.Value.Num());
	}
}

void UAurixNetDriver::SendPacketTo(uint16 Actor, TArrayView<const uint8> Bytes)
{
	UAurixNetSubsystem* S = Subsystem();
	if (!S) return;
	const uint16 Actors[1] = { Actor };
	S->SendEventTo(PacketCode, Bytes, Actors, false);
	bSentThisTick = true;
}

void UAurixNetDriver::LowLevelSend(TSharedPtr<const FInternetAddr> Address, void* Data, int32 CountBits, FOutPacketTraits& Traits)
{
	// Connectionless sends (stateless handshake) go by address.
	if (!Address.IsValid()) return;
	const uint16 Actor = static_cast<const FInternetAddrAurix&>(*Address).Actor;
	const uint8* Bytes = reinterpret_cast<const uint8*>(Data);
	int32 CountBytes = FMath::DivideAndRoundUp(CountBits, 8);
	if (ConnectionlessHandler.IsValid())
	{
		const ProcessedPacket Out = ConnectionlessHandler->OutgoingConnectionless(Address, reinterpret_cast<uint8*>(Data), CountBits, Traits);
		if (Out.bError) return;
		Bytes = Out.Data; CountBytes = FMath::DivideAndRoundUp(Out.CountBits, 8);
	}
	if (CountBytes > 0) SendPacketTo(Actor, TArrayView<const uint8>(Bytes, CountBytes));
}

FString UAurixNetDriver::LowLevelGetNetworkNumber() { const UAurixNetSubsystem* S = Subsystem(); return S ? FString::Printf(TEXT("aurix:%d"), S->GetRoom().Actor) : TEXT("aurix:?"); }
bool UAurixNetDriver::IsNetResourceValid() { const UAurixNetSubsystem* S = Subsystem(); return S && S->HasClient(); }

void UAurixNetDriver::ForgetConnection(uint16 Actor) { Remotes.Remove(Actor); }

void UAurixNetDriver::HandlePlayerLeft(int32 Actor, bool bOffline) { OnPlayerLeft(Actor, bOffline); }
void UAurixNetDriver::HandleMasterChanged(int32 Master) { OnMasterChanged(Master); }
void UAurixNetDriver::HandleClosed(const FString& Reason) { OnClosed(Reason); }

void UAurixNetDriver::OnPlayerLeft(int32 Actor, bool bOffline)
{
	if (bOffline) return; // a reconnecting player keeps the slot; UE's own timeout decides
	// With host migration master_changed follows the server leaving. A closed server connection makes
	// the engine raise ConnectionLost and drop to the default map before the migration travel.
	if (!bListening && Actor == ServerActor && (bMigrating || UAurixNetSettings::Get()->bHostMigration)) return;
	if (TWeakObjectPtr<UAurixNetConnection>* Found = Remotes.Find(uint16(Actor)))
	{
		if (Found->IsValid()) Found->Get()->Close();
		Remotes.Remove(uint16(Actor));
	}
	if (!bListening && Actor == ServerActor && ServerConnection) ServerConnection->Close();
}

void UAurixNetDriver::OnMasterChanged(int32 Master)
{
	if (bListening || Master == ServerActor) return;
	if (UAurixNetSettings::Get()->bHostMigration && Subsystem()) { BeginHostMigration(Master); return; }
	if (ServerConnection) ServerConnection->Close();
}

// The master left. Everyone stays in the room: the new master reloads the current map as a listen
// server on the same room, the others reconnect to it a moment later. Game state is the game's job
// (Starting → save to room props, Done → restore), exactly like Fusion's host-migration callbacks.
void UAurixNetDriver::BeginHostMigration(int32 NewMaster)
{
	UAurixNetSubsystem* S = Subsystem();
	bMigrateAsHost = S->GetRoom().Actor == NewMaster;
	MigrationRoom = S->GetRoom().Name;
	MigrateIn = bMigrateAsHost ? 0.2f : 1.5f; // host first, clients after it listens
	bMigrating = true;
	UE_LOG(LogAurixNet, Log, TEXT("host migration: new master %d, we %s"), NewMaster, bMigrateAsHost ? TEXT("host") : TEXT("rejoin"));
	S->OnHostMigration.Broadcast(EAurixNetMigration::Starting, bMigrateAsHost);
	S->OnHostMigration.Broadcast(bMigrateAsHost ? EAurixNetMigration::BecomingHost : EAurixNetMigration::Rejoining, bMigrateAsHost);
	// The server connection stays open until the travel replaces this driver (see OnPlayerLeft);
	// TickDispatch drops incoming packets while bMigrating is set.
}

FString UAurixNetDriver::CurrentMapPath() const
{
	const UWorld* W = GetWorld();
	return (W && W->GetOutermost()) ? W->GetOutermost()->GetName() : FString();
}

void UAurixNetDriver::TravelForMigration()
{
	UWorld* W = GetWorld();
	UAurixNetSubsystem* S = Subsystem();
	if (!W || !S || !GEngine) return;
	S->bResumingAfterMigration = true;
	const FString Url = bMigrateAsHost
		? FString::Printf(TEXT("%s?listen?Room=%s"), *CurrentMapPath(), *MigrationRoom)
		: FString::Printf(TEXT("%s.aurix%s"), *MigrationRoom, *CurrentMapPath());
	// Deferred travel: this runs inside the driver's own tick, and the travel destroys the driver.
	GEngine->SetClientTravel(W, *Url, TRAVEL_Absolute);
}

void UAurixNetDriver::OnClosed(const FString& Reason)
{
	UE_LOG(LogAurixNet, Warning, TEXT("cell connection closed (%s); shutting the net driver"), *Reason);
	if (ServerConnection) ServerConnection->Close();
	for (UNetConnection* C : ClientConnections) if (C) C->Close();
}

void UAurixNetDriver::LowLevelDestroy()
{
	if (UAurixNetSubsystem* S = Subsystem())
	{
		if (RawHandle.IsValid()) S->OnRawEvent.Remove(RawHandle);
		S->OnPlayerLeft.RemoveDynamic(this, &UAurixNetDriver::HandlePlayerLeft);
		S->OnMasterChanged.RemoveDynamic(this, &UAurixNetDriver::HandleMasterChanged);
		S->OnClosed.RemoveDynamic(this, &UAurixNetDriver::HandleClosed);
	}
	Remotes.Empty();
	Super::LowLevelDestroy();
}
