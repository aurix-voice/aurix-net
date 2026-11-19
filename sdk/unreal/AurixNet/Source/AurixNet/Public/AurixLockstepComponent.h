#pragma once
#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "AurixNetTypes.h"
#include "AurixLockstepComponent.generated.h"

struct AurixNetLockstep;
class UAurixNetSubsystem;

USTRUCT(BlueprintType)
struct AURIXNET_API FAurixLockstepInput
{
	GENERATED_BODY()
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net|Lockstep") int32 Actor = -1;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Net|Lockstep") TArray<uint8> Data;
};

/** A confirmed tick: run your deterministic simulation with these inputs, then ReportHash(Tick, Hash). */
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixLockstepTick, int64, Tick, const TArray<FAurixLockstepInput>&, Inputs);
/** A reliable command (SendCommand) from Actor, delivered to everyone at Tick; fires before OnTick of that tick. */
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FAurixLockstepCommand, int64, Tick, int32, Actor, const TArray<uint8>&, Data);
/** The cell restored a checkpoint (late join / resync): load this state, OnTick continues with Tick. An empty state means the start of the game. */
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FAurixLockstepRestore, int64, Tick, int64, Hash, const TArray<uint8>&, State);
/** The cell asks for a checkpoint (or a full dump): answer with SendCheckpoint / SendDump. */
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixLockstepStateRequest, int64, Tick, bool, bFullDump);

/**
 * Deterministic lockstep room client (docs/modes.md). Inputs go to the cell stamped with the tick,
 * confirmed ticks come back with every player's inputs in a fixed order; the component keeps the
 * adaptive input delay and redundancy of libaurix_net and exposes stats. Attach to any actor in a
 * lockstep room; the game instance's UAurixNetSubsystem must be in that room.
 */
UCLASS(ClassGroup = (AurixNet), meta = (BlueprintSpawnableComponent))
class AURIXNET_API UAurixLockstepComponent : public UActorComponent
{
	GENERATED_BODY()
public:
	UAurixLockstepComponent();
	virtual void BeginPlay() override;
	virtual void EndPlay(const EEndPlayReason::Type Reason) override;
	virtual void TickComponent(float DeltaTime, ELevelTick TickType, FActorComponentTickFunction* ThisTickFunction) override;

	/** Queue our input for the next tick. Returns the stamped tick or -1. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Lockstep") int64 SendInput(const TArray<uint8>& Data);
	/** Report the state hash after simulating Tick; the cell detects desyncs and can referee. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Lockstep") bool ReportHash(int64 Tick, int64 Hash);
	/** Reliable command outside the input stream (buy, surrender, pause): the cell stamps it onto the next tick; everyone (and the referee) gets it at that tick via OnCommand; journaled and replayed. 1..251 bytes (AURIX_NET_MAX_COMMAND); a longer one is refused: false, with the reason in the subsystem's GetLastError. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Lockstep") bool SendCommand(const TArray<uint8>& Data);
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Lockstep") FAurixLockstepCommand OnCommand;
	/** Answer a checkpoint request with the state before Tick: a late joiner loads it and gets the inputs from Tick on. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Lockstep") bool SendCheckpoint(int64 Tick, int64 Hash, const TArray<uint8>& State);
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Lockstep") bool SendDump(int64 Tick, int64 Hash, const TArray<uint8>& State);
	/** FNV-1a of a byte buffer, the same function the cell and other SDKs use for state hashes. */
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Lockstep") static int64 HashBytes(const TArray<uint8>& Data);

	/** Tuning / stats keys of libaurix_net (AURIX_NET_LS_*): 0 input delay, 1/2 min/max, 3 adaptive, 4 redundancy, 5 last margin, 6 late inputs, 7 confirmed tick, 8 server tick, 9 delivered, 10 catch-ups, 11 dropped. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Lockstep") int64 GetStat(int32 Key) const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Lockstep") bool SetOption(int32 Key, int64 Value);
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Lockstep") int64 ConfirmedTick() const;

	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Lockstep") FAurixLockstepTick OnTick;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Lockstep") FAurixLockstepRestore OnRestore;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Lockstep") FAurixLockstepStateRequest OnStateRequest;

private:
	UAurixNetSubsystem* Net() const;
	void OnRawData(uint8 Channel, TArrayView<const uint8> Data);
	AurixNetLockstep* Ls = nullptr;
	FDelegateHandle DataHandle;
};
