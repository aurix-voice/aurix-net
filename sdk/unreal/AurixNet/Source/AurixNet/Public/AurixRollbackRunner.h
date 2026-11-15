#pragma once
#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "AurixLockstepComponent.h"
#include "AurixRollbackRunner.generated.h"

/** Simulate one tick with these inputs. bPredicted = inputs may still change (local prediction). */
DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams(FAurixRollbackSimulate, int64, Tick, const TArray<FAurixLockstepInput>&, Inputs, bool, bPredicted);
/** Save the whole deterministic state (the runner stores it as the rollback point). */
DECLARE_DYNAMIC_DELEGATE_RetVal(TArray<uint8>, FAurixRollbackSave);
/** Load a state saved by Save. */
DECLARE_DYNAMIC_DELEGATE_OneParam(FAurixRollbackLoad, const TArray<uint8>&, State);
/** State hash after a confirmed tick (FNV-1a of Save output when unbound). */
DECLARE_DYNAMIC_DELEGATE_RetVal(int64, FAurixRollbackHash);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixRollbackResimulated, int64, FromTick, int32, Ticks);

/**
 * Predict-and-rollback over AurixLockstepComponent (Quantum-style): the local player's input is
 * applied immediately with the other players' last known inputs; when the cell confirms a tick the
 * runner restores the last confirmed state, applies the confirmed inputs, re-simulates the predicted
 * ticks, reports the hash, and records a replay. Bind Simulate / Save / Load (and Hash for custom
 * state) to your deterministic game code — an AurixSimWorld2D/3D fits directly (Snapshot/Restore/Hash).
 */
UCLASS(ClassGroup = (AurixNet), meta = (BlueprintSpawnableComponent))
class AURIXNET_API UAurixRollbackRunner : public UActorComponent
{
	GENERATED_BODY()
public:
	UAurixRollbackRunner();
	virtual void BeginPlay() override;
	virtual void EndPlay(const EEndPlayReason::Type Reason) override;
	virtual void TickComponent(float DeltaTime, ELevelTick TickType, FActorComponentTickFunction* ThisTickFunction) override;

	/** Lockstep component to drive (defaults to one on the same actor). */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Rollback") UAurixLockstepComponent* Lockstep = nullptr;
	/** Maximum ticks ahead of the confirmed tick; beyond it the runner waits (stalls) instead of predicting further. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Rollback", meta = (ClampMin = 0, ClampMax = 60)) int32 MaxPrediction = 8;
	/** Simulation rate; must equal the room's tick_hz. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Rollback", meta = (ClampMin = 1, ClampMax = 240)) int32 TickHz = 60;
	/** Record confirmed inputs for ExportReplay. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Rollback") bool bRecordReplay = true;

	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Rollback") FAurixRollbackSimulate OnSimulate;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Rollback") FAurixRollbackResimulated OnResimulated;
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Rollback") FAurixRollbackSave SaveState;
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Rollback") FAurixRollbackLoad LoadState;
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Rollback") FAurixRollbackHash HashState;

	/** Start at Tick with the current state as the confirmed baseline (call once the world is set up). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Rollback") void Start(int64 FromTick = 0);
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Rollback") void Stop();
	/** Our input for the next predicted tick (sent to the cell and applied locally). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Rollback") void SetLocalInput(const TArray<uint8>& Input);
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Rollback") int64 PredictedTick() const { return Predicted; }
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Rollback") int64 ConfirmedTick() const { return Confirmed; }
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Rollback") int32 Rollbacks() const { return RollbackCount; }
	/** Replay bytes: magic "AXRP", tick_hz, start tick, then per tick u16 n + (u16 actor, u16 len, bytes)*. Pair with the initial state snapshot. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Rollback") TArray<uint8> ExportReplay() const;
	/** Play a replay offline: restores InitialState and simulates every recorded tick through OnSimulate (bPredicted = false). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Rollback") bool PlayReplay(const TArray<uint8>& Replay, const TArray<uint8>& InitialState);

private:
	UFUNCTION() void HandleTick(int64 Tick, const TArray<FAurixLockstepInput>& Inputs);
	UFUNCTION() void HandleRestore(int64 Tick, int64 Hash, const TArray<uint8>& State);
	UFUNCTION() void HandleStateRequest(int64 Tick, bool bFullDump);
	void Simulate(int64 Tick, const TArray<FAurixLockstepInput>& Inputs, bool bPredicted);
	TArray<FAurixLockstepInput> PredictedInputs() const;
	int32 LocalActor() const;
	int64 CurrentHash();

	bool bRunning = false;
	int64 Confirmed = -1;   // last confirmed tick applied to ConfirmedState
	int64 Predicted = -1;   // last tick simulated (>= Confirmed)
	int32 RollbackCount = 0;
	double Accum = 0.0;
	TArray<uint8> ConfirmedState;
	TArray<uint8> LocalInput;
	TMap<int32, TArray<uint8>> LastInputs;                 // other actors' latest confirmed input (prediction source)
	TMap<int64, TArray<FAurixLockstepInput>> Pending;       // confirmed ticks received ahead of Confirmed+1
	TArray<TPair<int64, TArray<FAurixLockstepInput>>> Journal; // replay
	int64 JournalStart = 0;
};
