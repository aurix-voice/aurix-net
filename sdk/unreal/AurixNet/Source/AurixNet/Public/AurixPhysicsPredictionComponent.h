#pragma once
#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "AurixPhysicsPredictionComponent.generated.h"

class UPrimitiveComponent;
class UAurixNetSubsystem;

UENUM(BlueprintType)
enum class EAurixPhysicsMode : uint8
{
	/** Actor is replicated by the engine over AurixNetDriver: use Chaos predictive interpolation (5.4+) / resimulation (5.5+). */
	EngineReplication,
	/** No NetDriver: the owner streams physics snapshots as relay events; receivers extrapolate and correct smoothly. */
	RelayEvents
};

/**
 * Physics prediction for relay rooms. EngineReplication selects the engine's own client prediction
 * for replicated rigid bodies; RelayEvents is a
 * stand-alone path for rooms without a NetDriver: the owning player sends position / rotation /
 * velocities at SendHz (event code 201, optional interest group), everyone else integrates
 * velocities between snapshots and blends the error away over CorrectionTime.
 */
UCLASS(ClassGroup = (AurixNet), meta = (BlueprintSpawnableComponent))
class AURIXNET_API UAurixPhysicsPredictionComponent : public UActorComponent
{
	GENERATED_BODY()
public:
	UAurixPhysicsPredictionComponent();
	virtual void BeginPlay() override;
	virtual void EndPlay(const EEndPlayReason::Type Reason) override;
	virtual void TickComponent(float DeltaTime, ELevelTick TickType, FActorComponentTickFunction* ThisTickFunction) override;

	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Physics") EAurixPhysicsMode Mode = EAurixPhysicsMode::EngineReplication;
	/** Resimulation (full rollback of the physics scene, 5.5+) instead of predictive interpolation; falls back when unavailable. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Physics") bool bPreferResimulation = false;
	/** RelayEvents: snapshots per second from the owner. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Physics", meta = (ClampMin = 1, ClampMax = 120)) int32 SendHz = 20;
	/** RelayEvents: seconds to blend a received correction in (0 = snap). */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Physics", meta = (ClampMin = 0, ClampMax = 1)) float CorrectionTime = 0.1f;
	/** RelayEvents: beyond this distance the body snaps instead of blending. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Physics", meta = (ClampMin = 0)) float SnapDistance = 300.f;
	/** RelayEvents: interest group for the snapshots (0 = everyone; pair with AurixInterestComponent). */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Physics") int32 Group = 0;
	/** RelayEvents: true on the player that simulates this body (others receive). */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Physics") bool bOwner = false;
	/** RelayEvents: stable id shared by all players for this body (e.g. a spawn index); 0 = derive from the actor name. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Physics") int32 BodyId = 0;

	/** Event code used by RelayEvents snapshots. */
	static constexpr uint8 SnapshotCode = 201;

	UFUNCTION(BlueprintPure, Category = "Aurix Net|Physics") float LastCorrectionError() const { return LastError; }

private:
	UPrimitiveComponent* Body() const;
	UAurixNetSubsystem* Net() const;
	void ApplyEngineMode();
	void SendSnapshot();
	void OnRawEvent(const struct FAurixNetEventFrame& F);
	uint32 EffectiveId() const;

	FDelegateHandle RawHandle;
	float Accum = 0.f;
	// correction state (receivers)
	bool bHaveTarget = false;
	FVector TargetPos = FVector::ZeroVector, TargetVel = FVector::ZeroVector, TargetAngVel = FVector::ZeroVector;
	FQuat TargetRot = FQuat::Identity;
	FVector PosError = FVector::ZeroVector;
	FQuat RotError = FQuat::Identity;
	float Blend = 0.f;
	float LastError = 0.f;
};
