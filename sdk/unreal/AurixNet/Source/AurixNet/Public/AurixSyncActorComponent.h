#pragma once
#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "AurixSyncActorComponent.generated.h"

class UAurixSyncWorld;
class UAurixNetSubsystem;

/** Per-entity ownership policy: what the cell does when the owner leaves and who may take the entity. */
UENUM(BlueprintType)
enum class EAurixOwnerPolicy : uint8
{
	/** Despawned with its owner. */
	DestroyOnLeave = 0,
	/** Handed to the room master (oldest remaining player) when the owner leaves. */
	ToMaster = 1,
	/** Stays alive unowned; anyone may RequestOwnership. */
	Orphan = 2,
	/** Anyone may take it at any time (physics hand-offs). */
	Dynamic = 3
};

DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FAurixSyncActorBound, int64, Entity);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam(FAurixSyncActorDenied, int64, Entity);
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixSyncActorOwner, int32, Owner, bool, bLocal);

/**
 * Shared-mode replication for one actor over a sync room, without codegen: list the UPROPERTYs of
 * the owning actor (any class) that match fields of a schema component by name; the owner copies
 * their values into the room whenever they change (owner-authoritative writes), everyone else
 * applies the cell's deltas back into the same properties. Ownership follows the entity: spawn it
 * (bSpawnOnBeginPlay), bind to an existing one (BindEntity), or hand it over (TransferTo).
 *
 * Property types: bool, uint8/int32/int64 and enums (bool/uint/int/quant fields), float/double
 * (f32/quant), FVector/FVector2D (vec3/vec2), FRotator/FQuat (quat), FString/FName (string),
 * TArray<uint8> (bytes).
 */
UCLASS(ClassGroup = (AurixNet), meta = (BlueprintSpawnableComponent))
class AURIXNET_API UAurixSyncActorComponent : public UActorComponent
{
	GENERATED_BODY()
public:
	UAurixSyncActorComponent();
	virtual void BeginPlay() override;
	virtual void EndPlay(const EEndPlayReason::Type Reason) override;
	virtual void TickComponent(float DeltaTime, ELevelTick TickType, FActorComponentTickFunction* ThisTickFunction) override;

	/** Sync world to use; empty = the first AurixSyncWorld found in the level. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Sync") UAurixSyncWorld* World = nullptr;
	/** Schema component this actor maps to. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Sync") FString SchemaComponent;
	/** UPROPERTY names on the owning actor; each must exist as a field of SchemaComponent (same name). Empty = every field with a same-named property. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Sync") TArray<FName> Properties;
	/** Also map the root component's location/rotation to fields named `position` / `rotation` when the schema has them. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Sync") bool bSyncTransform = true;
	/** Applied to our entity once spawned (owner only); change later with SetPolicy. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Sync") EAurixOwnerPolicy OwnershipPolicy = EAurixOwnerPolicy::DestroyOnLeave;
	/** Spawn a new entity we own on BeginPlay (the typical "my pawn" case). Otherwise call BindEntity. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Sync") bool bSpawnOnBeginPlay = false;
	/** Despawn our entity when the component ends play (owner only); remote despawn destroys the actor. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Sync") bool bDespawnOnEndPlay = true;
	/** Owner: max writes per second (0 = every tick a value changed). */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Sync", meta = (ClampMin = 0, ClampMax = 120)) int32 WriteHz = 30;

	/** Bind to an existing entity (e.g. from AurixSyncWorld.OnSpawn for remote players' actors). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") void BindEntity(int64 Entity);
	/** Spawn an entity we own with the current property values; bound when the cell confirms it. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool Spawn();
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool Despawn();
	/** Hand the entity to another player (owner only). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool TransferTo(int32 Actor);
	/** Owner only: change the entity's ownership policy. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool SetPolicy(EAurixOwnerPolicy Policy);
	/** Ask the cell for ownership (unowned entity, or Dynamic policy). OnOwnerChanged(bLocal = true) on success, OnOwnershipDenied after RequestTimeout. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool RequestOwnership();
	/** Owner only: drop ownership; the entity stays alive unowned. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool Release();
	/** Seconds to wait for the cell before a request counts as denied. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Sync", meta = (ClampMin = 0.1)) float RequestTimeout = 1.0f;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Sync") FAurixSyncActorDenied OnOwnershipDenied;
	/** Force a full write on the next tick (owner). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") void MarkDirty() { bDirty = true; }

	UFUNCTION(BlueprintPure, Category = "Aurix Net|Sync") int64 GetEntity() const { return Entity; }
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Sync") bool IsBound() const { return Entity != 0; }
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Sync") bool IsOwner() const;
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Sync") int32 GetOwnerActor() const { return OwnerActor; }

	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Sync") FAurixSyncActorBound OnBound;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Sync") FAurixSyncActorOwner OnOwnerChanged;

private:
	struct FMap { FProperty* Prop = nullptr; int32 Field = -1; };
	UAurixNetSubsystem* Net() const;
	bool Resolve();
	void ApplyFromWorld();
	bool WriteToWorld(bool bForSpawn);
	bool WriteProperty(const FMap& M, void* Container);
	bool ReadProperty(const FMap& M, void* Container);
	UFUNCTION() void HandleSpawn(int64 InEntity, int32 Owner);
	UFUNCTION() void HandleChange(int64 InEntity, int32 Owner);
	UFUNCTION() void HandleDespawn(int64 InEntity, int32 Owner);

	int64 Entity = 0;
	int32 OwnerActor = -1;
	int32 Component = -1;
	int32 PosField = -1, RotField = -1;
	TArray<FMap> Maps;
	TArray<uint8> LastWritten;
	bool bDirty = true, bAwaitingSpawn = false, bResolved = false, bPolicySent = false;
	float RequestLeft = 0.f;
	float Accum = 0.f;
};
