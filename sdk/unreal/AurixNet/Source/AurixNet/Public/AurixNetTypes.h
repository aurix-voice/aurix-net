#pragma once
#include "CoreMinimal.h"
#include "AurixNetTypes.generated.h"

/** Room modes of Aurix Net (docs/modes.md). The NetDriver uses Relay; Sync and Lockstep have their own components. */
UENUM(BlueprintType)
enum class EAurixNetRoomMode : uint8 { Relay, Sync, Lockstep };

UENUM(BlueprintType)
enum class EAurixNetChannel : uint8
{
	/** Unreliable; a message that arrives after a newer one is dropped. */
	Unreliable = 0,
	Reliable = 1,
	ReliableUnordered = 2,
	/** Unreliable; every message that arrives is delivered, in the order of arrival. */
	UnreliableUnsequenced = 4
};

UENUM(BlueprintType)
enum class EAurixNetTarget : uint8 { All = 0, Others = 1, Master = 2 };

UENUM(BlueprintType)
enum class EAurixNetCache : uint8 { None = 0, Add = 1, Remove = 2 };

/** Host migration phases (relay NetDriver). */
UENUM(BlueprintType)
enum class EAurixNetMigration : uint8
{
	/** The master left; save what the new host must restore (room props are a good place). */
	Starting,
	/** We are the new master and are about to listen on the same room (level reload). */
	BecomingHost,
	/** A new master is listening; we are about to reconnect to it (level reload). */
	Rejoining,
	/** Migration finished; restore state (new host) or continue (client). */
	Done
};

UENUM(BlueprintType)
enum class EAurixNetState : uint8 { Disconnected, Connecting, Connected, InRoom, Closed };

/** Why a call failed. The values are the AURIX_NET_ERR_* codes of the native library. */
UENUM(BlueprintType)
enum class EAurixNetError : uint8
{
	None = 0,
	InvalidArgument = 1,
	Http = 2,
	Connection = 3,
	DataPath = 4,
	Disconnected = 5,
	Timeout = 6,
	Protocol = 7,
	Unauthorized = 100,
	Forbidden = 101,
	NotFound = 102,
	Conflict = 103,
	InvalidRequest = 104,
	RoomFull = 105,
	RoomClosed = 106,
	SchemaMismatch = 107,
	QuotaExceeded = 108,
	RateLimited = 109,
	Server = 199,
};

USTRUCT(BlueprintType)
struct AURIXNET_API FAurixNetPlayer
{
	GENERATED_BODY()
	/** Actor id inside the room (stable for the stay). */
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") int32 Actor = -1;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") FString PlayerId;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") FString Name;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") bool bOnline = true;
};

USTRUCT(BlueprintType)
struct AURIXNET_API FAurixNetRoomInfo
{
	GENERATED_BODY()
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") FString RoomId;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") FString Name;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") EAurixNetRoomMode Mode = EAurixNetRoomMode::Relay;
	/** Our actor id. */
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") int32 Actor = -1;
	/** The master (host) actor id. */
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") int32 Master = -1;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") int32 MaxPlayers = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") TArray<FAurixNetPlayer> Players;
};

USTRUCT(BlueprintType)
struct AURIXNET_API FAurixNetRoomSummary
{
	GENERATED_BODY()
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") FString RoomId;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") FString Name;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") EAurixNetRoomMode Mode = EAurixNetRoomMode::Relay;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") int32 Players = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") int32 MaxPlayers = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") bool bOpen = true;
	/** Lobby props as JSON text. */
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net") FString PropsJson;
};

/** A relay event frame as received: code, sender actor, payload. */
struct AURIXNET_API FAurixNetEventFrame
{
	uint8 Code = 0;
	uint16 From = 0;
	TArrayView<const uint8> Payload;

	/** Relay event frame: u8 code, u8 flags, u16 from (BE), [u16 group], [u16 n, n*u16], payload. */
	static bool Decode(const uint8* B, SIZE_T N, FAurixNetEventFrame& Out)
	{
		if (!B || N < 4) return false;
		Out.Code = B[0]; const uint8 Flags = B[1]; Out.From = uint16((B[2] << 8) | B[3]); SIZE_T O = 4;
		if (Flags & 0x10) { if (N < O + 2) return false; O += 2; }
		if ((Flags & 3) == 3) { if (N < O + 2) return false; const SIZE_T C = (B[O] << 8) | B[O + 1]; O += 2 + C * 2; if (N < O) return false; }
		Out.Payload = TArrayView<const uint8>(B + O, int32(N - O));
		return true;
	}
};
