#pragma once
#include "CoreMinimal.h"
#include "Engine/NetDriver.h"
#include "AurixNetTypes.h"
#include "AurixNetDriver.generated.h"

class UAurixNetSubsystem;
class UAurixNetConnection;

/**
 * Unreal NetDriver over an Aurix relay room. The listen server (or dedicated server) is the room
 * master; every other player is a client connection. Unreal's replication, RPCs and the login
 * handshake are untouched: packets ride as relay events addressed to one actor.
 *
 * Travel URLs: a host opens `<map>?listen?Room=<room>` (joins or creates the room), a client opens
 * `<room>.aurix` (joins it; the master is the server). The game instance's UAurixNetSubsystem
 * must already be connected to a cell with a grant.
 */
UCLASS(transient, config = Engine)
class AURIXNET_API UAurixNetDriver : public UNetDriver
{
	GENERATED_BODY()
public:
	/** Relay event code carrying Unreal packets (200+ are reserved for the driver). */
	static constexpr uint8 PacketCode = 200;

	/** Actor id of the server (room master); valid on clients after InitConnect. */
	uint16 ServerActor = 0;

	virtual bool IsAvailable() const override;
	virtual bool InitBase(bool bInitAsClient, FNetworkNotify* InNotify, const FURL& URL, bool bReuseAddressAndPort, FString& Error) override;
	virtual bool InitConnect(FNetworkNotify* InNotify, const FURL& ConnectURL, FString& Error) override;
	virtual bool InitListen(FNetworkNotify* InNotify, FURL& LocalURL, bool bReuseAddressAndPort, FString& Error) override;
	virtual void TickDispatch(float DeltaTime) override;
	virtual void TickFlush(float DeltaSeconds) override;
	virtual void LowLevelSend(TSharedPtr<const FInternetAddr> Address, void* Data, int32 CountBits, FOutPacketTraits& Traits) override;
	virtual FString LowLevelGetNetworkNumber() override;
	virtual void LowLevelDestroy() override;
	virtual bool IsNetResourceValid() override;
	virtual class ISocketSubsystem* GetSocketSubsystem() override { return nullptr; }

	void SendPacketTo(uint16 Actor, TArrayView<const uint8> Bytes);
	void ForgetConnection(uint16 Actor);

private:
	UAurixNetSubsystem* Subsystem() const;
	void OnRawEvent(const FAurixNetEventFrame& Frame);
	UAurixNetConnection* AcceptRemote(uint16 Actor, TArray<uint8>& Packet);

	UFUNCTION() void OnPlayerLeft(int32 Actor, bool bOffline);
	UFUNCTION() void OnMasterChanged(int32 Master);
	UFUNCTION() void OnClosed(const FString& Reason);

	void OnRoomJoined(int32 Actor, EAurixNetError Code, const FString& JoinFailure, FString RoomName);
	void EnterRoom();
	void BeginHostMigration(int32 NewMaster);
	void TravelForMigration();
	FString CurrentMapPath() const;
	bool bMigrating = false;
	bool bMigrateAsHost = false;
	FString MigrationRoom;
	TMap<uint16, TWeakObjectPtr<UAurixNetConnection>> Remotes;
	TArray<TPair<uint16, TArray<uint8>>> Inbox;
	FDelegateHandle RawHandle;
	bool bListening = false;
	bool bJoining = false;
	bool bInRoom = false;
	bool bStartingJoin = false;
	FString JoinError;
	bool bSentThisTick = false;
};
