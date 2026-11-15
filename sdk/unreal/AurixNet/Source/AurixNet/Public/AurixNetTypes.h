#pragma once
#include "CoreMinimal.h"
#include "AurixNetTypes.generated.h"

/** Room modes of Aurix Net (docs/modes.md). The NetDriver uses Relay; Sync and Lockstep have their own components. */
UENUM(BlueprintType)
enum class EAurixNetRoomMode : uint8 { Relay, Sync, Lockstep };

UENUM(BlueprintType)
enum class EAurixNetChannel : uint8 { Unreliable = 0, Reliable = 1, ReliableUnordered = 2 };

UENUM(BlueprintType)
enum class EAurixNetTarget : uint8 { All = 0, Others = 1, Master = 2 };

UENUM(BlueprintType)
enum class EAurixNetCache : uint8 { None = 0, Add = 1, Remove = 2 };

UENUM(BlueprintType)
enum class EAurixNetState : uint8 { Disconnected, Connecting, Connected, InRoom, Closed };

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
