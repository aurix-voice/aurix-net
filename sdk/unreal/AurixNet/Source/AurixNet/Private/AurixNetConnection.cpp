#include "AurixNetConnection.h"
#include "AurixNetDriver.h"
#include "AurixNetLog.h"
#include "PacketHandler.h"

static const int32 AurixMaxPacket = 1200; // under one QUIC datagram after the relay frame header

void UAurixNetConnection::InitBase(UNetDriver* InDriver, FSocket* InSocket, const FURL& InURL, EConnectionState InState, int32 InMaxPacket, int32 InPacketOverhead)
{
	Super::InitBase(InDriver, InSocket, InURL, InState, InMaxPacket == 0 ? AurixMaxPacket : InMaxPacket, InPacketOverhead == 0 ? 8 : InPacketOverhead);
	// UE's own reliability runs on top of unreliable relay events, like a UDP socket.
	InitSendBuffer();
}

void UAurixNetConnection::InitRemoteConnection(UNetDriver* InDriver, FSocket* InSocket, const FURL& InURL, const FInternetAddr& InRemoteAddr, EConnectionState InState, int32 InMaxPacket, int32 InPacketOverhead)
{
	RemoteActor = static_cast<const FInternetAddrAurix&>(InRemoteAddr).Actor;
	RemoteAddr = MakeShared<FInternetAddrAurix>(RemoteActor);
	InitBase(InDriver, InSocket, InURL, InState, InMaxPacket, InPacketOverhead);
	SetClientLoginState(EClientLoginState::LoggingIn);
	SetExpectedClientLoginMsgType(NMT_Hello);
}

void UAurixNetConnection::InitLocalConnection(UNetDriver* InDriver, FSocket* InSocket, const FURL& InURL, EConnectionState InState, int32 InMaxPacket, int32 InPacketOverhead)
{
	// The server is the room master; the driver resolved it before creating us.
	RemoteActor = CastChecked<UAurixNetDriver>(InDriver)->ServerActor;
	RemoteAddr = MakeShared<FInternetAddrAurix>(RemoteActor);
	InitBase(InDriver, InSocket, InURL, InState, InMaxPacket, InPacketOverhead);
}

void UAurixNetConnection::LowLevelSend(void* Data, int32 CountBits, FOutPacketTraits& Traits)
{
	const uint8* Bytes = reinterpret_cast<const uint8*>(Data);
	int32 CountBytes = FMath::DivideAndRoundUp(CountBits, 8);
	if (Handler.IsValid() && !Handler->GetRawSend())
	{
		const ProcessedPacket Out = Handler->Outgoing(reinterpret_cast<uint8*>(Data), CountBits, Traits);
		if (Out.bError) return;
		Bytes = Out.Data; CountBytes = FMath::DivideAndRoundUp(Out.CountBits, 8);
	}
	if (CountBytes <= 0) return;
	UAurixNetDriver* D = Cast<UAurixNetDriver>(Driver);
	if (D) D->SendPacketTo(RemoteActor, TArrayView<const uint8>(Bytes, CountBytes));
}

FString UAurixNetConnection::LowLevelGetRemoteAddress(bool) { return FString::Printf(TEXT("aurix:%u"), RemoteActor); }
FString UAurixNetConnection::LowLevelDescribe() { return FString::Printf(TEXT("actor=%u state=%d"), RemoteActor, (int32)GetConnectionState()); }

void UAurixNetConnection::CleanUp()
{
	if (UAurixNetDriver* D = Cast<UAurixNetDriver>(Driver)) D->ForgetConnection(RemoteActor);
	Super::CleanUp();
}
