#pragma once
#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "AurixInterestComponent.generated.h"

class UAurixNetSubsystem;

/**
 * Grid interest management for relay rooms: the owner's position maps to a grid cell; the component
 * joins the interest groups of the cells within Radius and leaves the others as the actor moves.
 * Send position/state events with Group = CurrentGroup() so only nearby players receive them.
 */
UCLASS(ClassGroup = (AurixNet), meta = (BlueprintSpawnableComponent))
class AURIXNET_API UAurixInterestComponent : public UActorComponent
{
	GENERATED_BODY()
public:
	UAurixInterestComponent();
	virtual void BeginPlay() override;
	virtual void EndPlay(const EEndPlayReason::Type Reason) override;
	virtual void TickComponent(float DeltaTime, ELevelTick TickType, FActorComponentTickFunction* ThisTickFunction) override;

	/** Cell size in Unreal units; 0 = project setting. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Interest", meta = (ClampMin = 0)) float CellSize = 0.f;
	/** Cells around the owner's cell to subscribe (1 = 3x3). */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Interest", meta = (ClampMin = 0, ClampMax = 4)) int32 Radius = 1;
	/** Re-evaluate at most this often. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Interest", meta = (ClampMin = 0.05)) float UpdateInterval = 0.25f;

	/** Group id of the owner's current cell (1..65535), to pass as Group in SendEventEx. */
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Interest") int32 CurrentGroup() const { return CurrentCell; }
	/** Group id for a world position (same mapping as the owner's). */
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Interest") int32 GroupFor(FVector Location) const;
	/** Groups currently joined. */
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Interest") const TArray<int32>& JoinedGroups() const { return Joined; }

private:
	UAurixNetSubsystem* Net() const;
	float Cell() const;
	void Recompute(bool bForce);
	int32 CurrentCell = 0;
	TArray<int32> Joined;
	float Accum = 0.f;
};
