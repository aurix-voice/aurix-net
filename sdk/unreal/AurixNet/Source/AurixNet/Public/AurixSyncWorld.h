#pragma once
#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "AurixNetTypes.h"
#include "AurixSyncWorld.generated.h"

struct AurixNetSync;
class UAurixNetSubsystem;

/** Entity spawned / changed / despawned in the replicated world (Owner -1 = server-owned). */
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(FAurixSyncEntity, int64, Entity, int32, Owner);

/**
 * Client view of a sync room (docs/modes.md): the cell owns the world and streams deltas; this
 * component applies them, acks, and exposes entity/component/field reads plus owner-authoritative
 * writes. One per level in a sync room; the game instance's UAurixNetSubsystem must be in that
 * room with the same schema (schema hash is checked by the cell on join).
 */
UCLASS(ClassGroup = (AurixNet), meta = (BlueprintSpawnableComponent))
class AURIXNET_API UAurixSyncWorld : public UActorComponent
{
	GENERATED_BODY()
public:
	UAurixSyncWorld();
	virtual void BeginPlay() override;
	virtual void EndPlay(const EEndPlayReason::Type Reason) override;
	virtual void TickComponent(float DeltaTime, ELevelTick TickType, FActorComponentTickFunction* ThisTickFunction) override;

	/** Schema JSON (same text the room was created with). Set before BeginPlay or call Setup. */
	UPROPERTY(EditAnywhere, BlueprintReadWrite, Category = "Aurix Net|Sync", meta = (MultiLine = true)) FString SchemaJson;
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool Setup(const FString& InSchemaJson);

	UFUNCTION(BlueprintPure, Category = "Aurix Net|Sync") int64 ServerTick() const;
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Sync") int32 EntityCount() const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool EntityAt(int32 Index, int64& Entity, int32& Owner) const;
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Sync") int32 ComponentId(const FString& Name) const;
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Sync") int32 FieldIndex(int32 Component, const FString& Name) const;
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Sync") bool Has(int64 Entity, int32 Component) const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool GetNumber(int64 Entity, int32 Component, int32 Field, double& Out) const;
	/** vec3 / quantized position fields; Unreal units are up to the schema (cm vs m, Z-up). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool GetVector(int64 Entity, int32 Component, int32 Field, FVector& Out) const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool GetQuat(int64 Entity, int32 Component, int32 Field, FQuat& Out) const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool GetString(int64 Entity, int32 Component, int32 Field, FString& Out) const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool GetBytes(int64 Entity, int32 Component, int32 Field, TArray<uint8>& Out) const;

	/** Raw input for server logic (rooms with a WASM module). Returns the stamped tick or -1. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") int64 SendInput(const TArray<uint8>& Data);

	/** Owner-authoritative writes: Begin(component), Write*, End; then SendWrites or SpawnEntity / WriteEntity. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool WriteBegin(int32 Component);
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool WriteNumber(int32 Field, double Value);
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool WriteVector(int32 Field, FVector Value);
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool WriteQuat(int32 Field, FQuat Value);
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool WriteBytes(int32 Field, const TArray<uint8>& Value);
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") bool WriteEnd();
	/** Queued writes as this tick's input (owner-authoritative rooms). Returns stamped tick or -1. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") int64 SendWrites();
	/** Op 0 = write entity (queued writes), 1 = spawn (queued writes), 2 = despawn, 3 = transfer ownership to actor To. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Net|Sync") int64 SendOp(int32 Op, int64 Entity, int32 To = -1);

	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Sync") FAurixSyncEntity OnSpawn;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Sync") FAurixSyncEntity OnChange;
	UPROPERTY(BlueprintAssignable, Category = "Aurix Net|Sync") FAurixSyncEntity OnDespawn;

	/** Deltas missed since join (resyncs); non-zero means a snapshot was requested. */
	UFUNCTION(BlueprintPure, Category = "Aurix Net|Sync") int64 Gaps() const;

	bool IsValidWorld() const { return S != nullptr; }
private:
	UAurixNetSubsystem* Net() const;
	void OnRawData(uint8 Channel, TArrayView<const uint8> Data);
	void Teardown();
	AurixNetSync* S = nullptr;
	FDelegateHandle DataHandle;
};
