#include "AurixNetDriver.h"
#include "AurixNetConnection.h"
#include "AurixNetSubsystem.h"
#include "AurixNetLog.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "AurixNetSettings.h"
#include "Engine/Engine.h"
#include "UObject/Package.h"
#include "PacketHandler.h"
#include "PacketHandlers/StatelessConnectHandlerComponent.h"
#include "Net/Common/Packets/PacketView.h"
#include "SocketTypes.h"

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
	bListening = !bInitAsClient;
	if (S->GetState() == EAurixNetState::InRoom && (RoomName.IsEmpty() || S->GetRoom().Name == RoomName)) return true;

	// Like a socket that is still connecting: the travel goes on, IsNetResourceValid() holds the
	// handshake back until the room is joined, and a refused join closes the driver.
	if (S->GetState() == EAurixNetState::InRoom) S->Leave();
	const TCHAR* MaxPlayersOption = URL.GetOption(TEXT("MaxPlayers="), nullptr);
	const int32 Max = MaxPlayersOption ? FCString::Atoi(MaxPlayersOption) : UAurixNetSettings::Get()->DefaultMaxPlayers;
	const FString JoinJson = bInitAsClient ? UAurixNetSubsystem::JoinJsonFor(RoomName) : UAurixNetSubsystem::JoinOrCreateJson(RoomName, EAurixNetRoomMode::Relay, Max, FString());
	bJoining = true;
	bStartingJoin = true;
	S->JoinAsync(JoinJson, FAurixNetAsyncResult::CreateUObject(this, &UAurixNetDriver::OnRoomJoined, RoomName));
	bStartingJoin = false;
	if (!bJoining)
	{
		Error = FString::Printf(TEXT("Aurix Net: could not join room '%s': %s"), *RoomName, *JoinError);
		return false;
	}
	return true;
}

void UAurixNetDriver::OnRoomJoined(int32 Actor, EAurixNetError Code, const FString& JoinFailure, FString RoomName)
{
	bJoining = false;
	UAurixNetSubsystem* S = Subsystem();
	JoinError = Actor < 0 || !S ? JoinFailure : bListening && !S->IsMaster() ? TEXT("this player is not the room master; only the master can listen") : FString();
	if (JoinError.IsEmpty())
	{
		EnterRoom();
		return;
	}
	if (bStartingJoin) return;
	UE_LOG(LogAurixNet, Error, TEXT("could not join room '%s': %s"), *RoomName, *JoinError);
	if (ServerConnection)
	{
		ServerConnection->SetConnectionState(USOCK_Closed);
		return;
	}
	GEngine->BroadcastNetworkFailure(GetWorld(), this, ENetworkFailure::NetDriverListenFailure, JoinError);
	Shutdown();
}

void UAurixNetDriver::EnterRoom()
{
	UAurixNetSubsystem* S = Subsystem();
	bInRoom = true;
	RawHandle = S->OnRawEvent.AddUObject(this, &UAurixNetDriver::OnRawEvent);
	S->OnPlayerLeft.AddDynamic(this, &UAurixNetDriver::OnPlayerLeft);
	S->OnMasterChanged.AddDynamic(this, &UAurixNetDriver::OnMasterChanged);
	S->OnClosed.AddDynamic(this, &UAurixNetDriver::OnClosed);
	LocalAddr = MakeShared<FInternetAddrAurix>(uint16(S->GetRoom().Actor));
	if (UAurixNetConnection* C = Cast<UAurixNetConnection>(ServerConnection))
	{
		ServerActor = uint16(S->GetRoom().Master);
		C->SetRemoteActor(ServerActor);
		Remotes.Add(ServerActor, C);
		UE_LOG(LogAurixNet, Log, TEXT("client: joined room '%s' as actor %d, server is actor %u"), *S->GetRoom().Name, S->GetRoom().Actor, ServerActor);
	}
	else
	{
		UE_LOG(LogAurixNet, Log, TEXT("listen: room '%s', master actor %d"), *S->GetRoom().Name, S->GetRoom().Actor);
	}
	if (S->bResumingAfterMigration)
	{
		S->bResumingAfterMigration = false;
		S->OnHostMigration.Broadcast(EAurixNetMigration::Done, bListening);
	}
}

bool UAurixNetDriver::InitConnect(FNetworkNotify* InNotify, const FURL& ConnectURL, FString& Error)
{
	if (!InitBase(true, InNotify, ConnectURL, false, Error)) return false;
	ServerConnection = NewObject<UAurixNetConnection>(GetTransientPackage(), NetConnectionClass);
	ServerConnection->InitLocalConnection(this, nullptr, ConnectURL, USOCK_Pending);
	CreateInitialClientChannels();
	if (!bJoining) EnterRoom();
	return true;
}

bool UAurixNetDriver::InitListen(FNetworkNotify* InNotify, FURL& LocalURL, bool bReuseAddressAndPort, FString& Error)
{
	if (!InitBase(false, InNotify, LocalURL, bReuseAddressAndPort, Error)) return false;
	InitConnectionlessHandler();
	if (bJoining) return true;
	if (!Subsystem()->IsMaster())
	{
		Error = TEXT("Aurix Net: this player is not the room master; only the master can listen");
		return false;
	}
	EnterRoom();
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
		C = NewObject<UAurixNetConnection>(GetTransientPackage(), NetConnectionClass);
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
	if (bMigrating) return;
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

void UAurixNetDriver::TickFlush(float DeltaSeconds)
{
	Super::TickFlush(DeltaSeconds);
	// The relay client batches unreliable sends; without this they wait for its own timer.
	if (bSentThisTick)
	{
		bSentThisTick = false;
		if (UAurixNetSubsystem* S = Subsystem())
		{
			S->Flush();
		}
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
bool UAurixNetDriver::IsNetResourceValid() { const UAurixNetSubsystem* S = Subsystem(); return S && S->HasClient() && bInRoom; }

void UAurixNetDriver::ForgetConnection(uint16 Actor) { Remotes.Remove(Actor); }


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
// server on the same room and the others reconnect to it; their handshake is resent until it
// listens. Game state is the game's job (save it to the room props on Starting, restore it on Done).
void UAurixNetDriver::BeginHostMigration(int32 NewMaster)
{
	UAurixNetSubsystem* S = Subsystem();
	bMigrateAsHost = S->GetRoom().Actor == NewMaster;
	MigrationRoom = S->GetRoom().Name;
	bMigrating = true;
	UE_LOG(LogAurixNet, Log, TEXT("host migration: new master %d, we %s"), NewMaster, bMigrateAsHost ? TEXT("host") : TEXT("rejoin"));
	S->OnHostMigration.Broadcast(EAurixNetMigration::Starting, bMigrateAsHost);
	S->OnHostMigration.Broadcast(bMigrateAsHost ? EAurixNetMigration::BecomingHost : EAurixNetMigration::Rejoining, bMigrateAsHost);
	// The server connection stays open until the travel replaces this driver (see OnPlayerLeft);
	// TickDispatch drops incoming packets while bMigrating is set.
	TravelForMigration();
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
	// The engine runs the travel on its next tick; it destroys this driver.
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
		S->OnPlayerLeft.RemoveDynamic(this, &UAurixNetDriver::OnPlayerLeft);
		S->OnMasterChanged.RemoveDynamic(this, &UAurixNetDriver::OnMasterChanged);
		S->OnClosed.RemoveDynamic(this, &UAurixNetDriver::OnClosed);
	}
	Remotes.Empty();
	Super::LowLevelDestroy();
}
