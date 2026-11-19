#pragma once
#include "CoreMinimal.h"
#include "Engine/NetConnection.h"
#include "IPAddress.h"
#include "AurixNetConnection.generated.h"

/** An address inside an Aurix room: the remote actor id. */
class FInternetAddrAurix : public FInternetAddr
{
public:
	uint16 Actor = 0;
	FInternetAddrAurix() = default;
	explicit FInternetAddrAurix(uint16 InActor) : Actor(InActor) {}
	virtual TArray<uint8> GetRawIp() const override { return { uint8(Actor >> 8), uint8(Actor & 0xFF) }; }
	virtual void SetRawIp(const TArray<uint8>& R) override { Actor = R.Num() >= 2 ? uint16((R[0] << 8) | R[1]) : 0; }
	virtual void SetIp(uint32 InAddr) override { Actor = uint16(InAddr & 0xFFFF); }
	virtual void SetIp(const TCHAR* InAddr, bool& bIsValid) override { Actor = uint16(FCString::Atoi(InAddr)); bIsValid = true; }
	virtual void GetIp(uint32& OutAddr) const override { OutAddr = Actor; }
	virtual void SetPort(int32) override {}
	virtual int32 GetPort() const override { return 0; }
	virtual void SetAnyAddress() override { Actor = 0; }
	virtual void SetBroadcastAddress() override { Actor = 0xFFFF; }
	virtual void SetLoopbackAddress() override { Actor = 0; }
	virtual FString ToString(bool) const override { return FString::Printf(TEXT("aurix:%u"), Actor); }
	virtual bool operator==(const FInternetAddr& O) const override { const FInternetAddrAurix* A = static_cast<const FInternetAddrAurix*>(&O); return A && A->Actor == Actor; }
	virtual uint32 GetTypeHash() const override { return ::GetTypeHash(Actor); }
	virtual bool IsValid() const override { return true; }
	virtual TSharedRef<FInternetAddr> Clone() const override { return MakeShared<FInternetAddrAurix>(Actor); }
	virtual FName GetProtocolType() const override { return FName(TEXT("AurixNet")); }
};

/** One remote peer of the room; packets travel as relay events addressed to that actor. */
UCLASS(transient, config = Engine)
class AURIXNET_API UAurixNetConnection : public UNetConnection
{
	GENERATED_BODY()
public:
	/** Remote actor id inside the room. */
	uint16 RemoteActor = 0;
	void SetRemoteActor(uint16 Actor);

	virtual void InitBase(UNetDriver* InDriver, class FSocket* InSocket, const FURL& InURL, EConnectionState InState, int32 InMaxPacket = 0, int32 InPacketOverhead = 0) override;
	virtual void InitRemoteConnection(UNetDriver* InDriver, class FSocket* InSocket, const FURL& InURL, const FInternetAddr& InRemoteAddr, EConnectionState InState, int32 InMaxPacket = 0, int32 InPacketOverhead = 0) override;
	virtual void InitLocalConnection(UNetDriver* InDriver, class FSocket* InSocket, const FURL& InURL, EConnectionState InState, int32 InMaxPacket = 0, int32 InPacketOverhead = 0) override;
	virtual void LowLevelSend(void* Data, int32 CountBits, FOutPacketTraits& Traits) override;
	virtual FString LowLevelGetRemoteAddress(bool bAppendPort = false) override;
	virtual FString LowLevelDescribe() override;
	virtual void CleanUp() override;
};
