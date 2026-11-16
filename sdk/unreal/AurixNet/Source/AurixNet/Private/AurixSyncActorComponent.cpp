#include "AurixSyncActorComponent.h"
#include "AurixSyncWorld.h"
#include "AurixNetSubsystem.h"
#include "AurixNetLog.h"
#include "GameFramework/Actor.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "EngineUtils.h"
#include "UObject/UnrealType.h"
#include "UObject/EnumProperty.h"
#include "Serialization/MemoryWriter.h"

UAurixSyncActorComponent::UAurixSyncActorComponent() { PrimaryComponentTick.bCanEverTick = true; }

UAurixNetSubsystem* UAurixSyncActorComponent::Net() const
{
	const UWorld* W = GetWorld(); const UGameInstance* GI = W ? W->GetGameInstance() : nullptr;
	return GI ? GI->GetSubsystem<UAurixNetSubsystem>() : nullptr;
}
bool UAurixSyncActorComponent::IsOwner() const { const UAurixNetSubsystem* N = Net(); return N && Entity != 0 && OwnerActor >= 0 && OwnerActor == N->GetRoom().Actor; }

bool UAurixSyncActorComponent::Resolve()
{
	if (bResolved) return true;
	if (!World) { for (TActorIterator<AActor> It(GetWorld()); It && !World; ++It) World = It->FindComponentByClass<UAurixSyncWorld>(); }
	AActor* O = GetOwner();
	if (!World || !World->IsValidWorld() || !O) return false;
	Component = World->ComponentId(SchemaComponent);
	if (Component < 0) { UE_LOG(LogAurixNet, Warning, TEXT("%s: schema component '%s' not found"), *GetNameSafe(O), *SchemaComponent); return false; }
	Maps.Reset();
	auto TryMap = [&](FProperty* P) { const int32 F = World->FieldIndex(Component, P->GetName()); if (F < 0) return false; FMap M; M.Prop = P; M.Field = F; Maps.Add(M); return true; };
	if (Properties.Num())
	{
		for (const FName& N : Properties)
		{
			FProperty* P = O->GetClass()->FindPropertyByName(N);
			if (!P) { UE_LOG(LogAurixNet, Warning, TEXT("%s: no property '%s'"), *GetNameSafe(O), *N.ToString()); continue; }
			if (!TryMap(P)) UE_LOG(LogAurixNet, Warning, TEXT("%s: schema '%s' has no field '%s'"), *GetNameSafe(O), *SchemaComponent, *N.ToString());
		}
	}
	else { for (TFieldIterator<FProperty> It(O->GetClass()); It; ++It) TryMap(*It); }
	if (bSyncTransform) { PosField = World->FieldIndex(Component, TEXT("position")); RotField = World->FieldIndex(Component, TEXT("rotation")); }
	bResolved = true;
	return true;
}

void UAurixSyncActorComponent::BeginPlay()
{
	Super::BeginPlay();
	if (Resolve() && World)
	{
		World->OnSpawn.AddDynamic(this, &UAurixSyncActorComponent::HandleSpawn);
		World->OnChange.AddDynamic(this, &UAurixSyncActorComponent::HandleChange);
		World->OnDespawn.AddDynamic(this, &UAurixSyncActorComponent::HandleDespawn);
		if (bSpawnOnBeginPlay) Spawn();
	}
}
void UAurixSyncActorComponent::EndPlay(const EEndPlayReason::Type Reason)
{
	if (bDespawnOnEndPlay && IsOwner()) Despawn();
	if (World)
	{
		World->OnSpawn.RemoveDynamic(this, &UAurixSyncActorComponent::HandleSpawn);
		World->OnChange.RemoveDynamic(this, &UAurixSyncActorComponent::HandleChange);
		World->OnDespawn.RemoveDynamic(this, &UAurixSyncActorComponent::HandleDespawn);
	}
	Super::EndPlay(Reason);
}

void UAurixSyncActorComponent::BindEntity(int64 E)
{
	Entity = E; int64 Id; int32 Own = -1;
	for (int32 I = 0; World && World->EntityAt(I, Id, Own); ++I) if (Id == E) { OwnerActor = Own; break; }
	OnBound.Broadcast(Entity);
	OnOwnerChanged.Broadcast(OwnerActor, IsOwner());
	if (!IsOwner()) ApplyFromWorld();
}
bool UAurixSyncActorComponent::Spawn()
{
	if (!Resolve() || Entity != 0 || bAwaitingSpawn) return false;
	if (!WriteToWorld(true)) return false;
	bAwaitingSpawn = World->SendOp(1, 0, -1) >= 0;
	return bAwaitingSpawn;
}
bool UAurixSyncActorComponent::Despawn() { if (!IsOwner()) return false; const bool bOk = World->SendOp(2, Entity, -1) >= 0; Entity = 0; OwnerActor = -1; return bOk; }
bool UAurixSyncActorComponent::TransferTo(int32 Actor) { return IsOwner() && World->SendOp(3, Entity, Actor) >= 0; }

void UAurixSyncActorComponent::HandleSpawn(int64 E, int32 Owner)
{
	// Our own spawn comes back as a spawn owned by us; the first one while we wait is ours.
	if (bAwaitingSpawn && Entity == 0 && Net() && Owner == Net()->GetRoom().Actor)
	{
		bAwaitingSpawn = false; Entity = E; OwnerActor = Owner;
		OnBound.Broadcast(Entity); OnOwnerChanged.Broadcast(OwnerActor, true);
	}
}
void UAurixSyncActorComponent::HandleChange(int64 E, int32 Owner)
{
	if (E != Entity) return;
	if (Owner != OwnerActor) { OwnerActor = Owner; OnOwnerChanged.Broadcast(OwnerActor, IsOwner()); }
	if (!IsOwner()) ApplyFromWorld();
}
void UAurixSyncActorComponent::HandleDespawn(int64 E, int32)
{
	if (E != Entity) return;
	Entity = 0; OwnerActor = -1;
	if (bDespawnOnEndPlay && GetOwner()) GetOwner()->Destroy();
}

void UAurixSyncActorComponent::TickComponent(float Dt, ELevelTick T, FActorComponentTickFunction* F)
{
	Super::TickComponent(Dt, T, F);
	if (!IsOwner()) return;
	Accum += Dt;
	const float Step = WriteHz > 0 ? 1.f / WriteHz : 0.f;
	if (Accum < Step) return;
	Accum = 0.f;
	if (WriteToWorld(false)) World->SendOp(0, Entity, -1);
}

bool UAurixSyncActorComponent::WriteProperty(const FMap& M, void* C)
{
	FProperty* P = M.Prop; const void* V = P->ContainerPtrToValuePtr<void>(C);
	if (const FBoolProperty* B = CastField<FBoolProperty>(P)) return World->WriteNumber(M.Field, B->GetPropertyValue(V) ? 1 : 0);
	if (const FNumericProperty* N = CastField<FNumericProperty>(P)) return World->WriteNumber(M.Field, N->IsFloatingPoint() ? N->GetFloatingPointPropertyValue(V) : (double)N->GetSignedIntPropertyValue(V));
	if (const FEnumProperty* E = CastField<FEnumProperty>(P)) return World->WriteNumber(M.Field, (double)E->GetUnderlyingProperty()->GetSignedIntPropertyValue(V));
	if (const FStructProperty* S = CastField<FStructProperty>(P))
	{
		if (S->Struct == TBaseStructure<FVector>::Get()) return World->WriteVector(M.Field, *reinterpret_cast<const FVector*>(V));
		if (S->Struct == TBaseStructure<FVector2D>::Get()) { const FVector2D& X = *reinterpret_cast<const FVector2D*>(V); return World->WriteVector(M.Field, FVector(X.X, X.Y, 0)); }
		if (S->Struct == TBaseStructure<FQuat>::Get()) return World->WriteQuat(M.Field, *reinterpret_cast<const FQuat*>(V));
		if (S->Struct == TBaseStructure<FRotator>::Get()) return World->WriteQuat(M.Field, reinterpret_cast<const FRotator*>(V)->Quaternion());
		return false;
	}
	if (const FStrProperty* Str = CastField<FStrProperty>(P)) { const FTCHARToUTF8 U(*Str->GetPropertyValue(V)); TArray<uint8> B((const uint8*)U.Get(), U.Length()); return World->WriteBytes(M.Field, B); }
	if (const FNameProperty* Nm = CastField<FNameProperty>(P)) { const FTCHARToUTF8 U(*Nm->GetPropertyValue(V).ToString()); TArray<uint8> B((const uint8*)U.Get(), U.Length()); return World->WriteBytes(M.Field, B); }
	if (const FArrayProperty* A = CastField<FArrayProperty>(P)) { if (CastField<FByteProperty>(A->Inner)) return World->WriteBytes(M.Field, *reinterpret_cast<const TArray<uint8>*>(V)); }
	return false;
}
bool UAurixSyncActorComponent::ReadProperty(const FMap& M, void* C)
{
	FProperty* P = M.Prop; void* V = P->ContainerPtrToValuePtr<void>(C);
	double D = 0; FVector Vec; FQuat Q; TArray<uint8> B; FString S2;
	if (const FBoolProperty* Bp = CastField<FBoolProperty>(P)) { if (!World->GetNumber(Entity, Component, M.Field, D)) return false; Bp->SetPropertyValue(V, D != 0); return true; }
	if (const FNumericProperty* N = CastField<FNumericProperty>(P)) { if (!World->GetNumber(Entity, Component, M.Field, D)) return false; if (N->IsFloatingPoint()) N->SetFloatingPointPropertyValue(V, D); else N->SetIntPropertyValue(V, (int64)D); return true; }
	if (const FEnumProperty* E = CastField<FEnumProperty>(P)) { if (!World->GetNumber(Entity, Component, M.Field, D)) return false; E->GetUnderlyingProperty()->SetIntPropertyValue(V, (int64)D); return true; }
	if (const FStructProperty* S = CastField<FStructProperty>(P))
	{
		if (S->Struct == TBaseStructure<FVector>::Get()) { if (!World->GetVector(Entity, Component, M.Field, Vec)) return false; *reinterpret_cast<FVector*>(V) = Vec; return true; }
		if (S->Struct == TBaseStructure<FVector2D>::Get()) { if (!World->GetVector(Entity, Component, M.Field, Vec)) return false; *reinterpret_cast<FVector2D*>(V) = FVector2D(Vec.X, Vec.Y); return true; }
		if (S->Struct == TBaseStructure<FQuat>::Get()) { if (!World->GetQuat(Entity, Component, M.Field, Q)) return false; *reinterpret_cast<FQuat*>(V) = Q; return true; }
		if (S->Struct == TBaseStructure<FRotator>::Get()) { if (!World->GetQuat(Entity, Component, M.Field, Q)) return false; *reinterpret_cast<FRotator*>(V) = Q.Rotator(); return true; }
		return false;
	}
	if (const FStrProperty* Str = CastField<FStrProperty>(P)) { if (!World->GetString(Entity, Component, M.Field, S2)) return false; Str->SetPropertyValue(V, S2); return true; }
	if (const FNameProperty* Nm = CastField<FNameProperty>(P)) { if (!World->GetString(Entity, Component, M.Field, S2)) return false; Nm->SetPropertyValue(V, FName(*S2)); return true; }
	if (const FArrayProperty* A = CastField<FArrayProperty>(P)) { if (CastField<FByteProperty>(A->Inner) && World->GetBytes(Entity, Component, M.Field, B)) { *reinterpret_cast<TArray<uint8>*>(V) = B; return true; } }
	return false;
}

bool UAurixSyncActorComponent::WriteToWorld(bool bForSpawn)
{
	AActor* O = GetOwner();
	if (!O || !World) return false;
	// Change detection: serialise the mapped properties and compare with the last write.
	TArray<uint8> Now;
	{
		FMemoryWriter Ar(Now);
		for (const FMap& M : Maps) M.Prop->SerializeItem(FStructuredArchiveFromArchive(Ar).GetSlot(), M.Prop->ContainerPtrToValuePtr<void>(O));
		if (bSyncTransform && O->GetRootComponent()) { FVector L = O->GetActorLocation(); FQuat R = O->GetActorQuat(); Ar << L; Ar << R; }
	}
	if (!bForSpawn && !bDirty && Now == LastWritten) return false;
	if (!World->WriteBegin(Component)) return false;
	for (const FMap& M : Maps) WriteProperty(M, O);
	if (bSyncTransform && O->GetRootComponent())
	{
		if (PosField >= 0) World->WriteVector(PosField, O->GetActorLocation());
		if (RotField >= 0) World->WriteQuat(RotField, O->GetActorQuat());
	}
	World->WriteEnd();
	LastWritten = MoveTemp(Now); bDirty = false;
	return true;
}

void UAurixSyncActorComponent::ApplyFromWorld()
{
	AActor* O = GetOwner();
	if (!O || !World || Entity == 0 || !World->Has(Entity, Component)) return;
	for (const FMap& M : Maps) ReadProperty(M, O);
	if (bSyncTransform && O->GetRootComponent())
	{
		FVector L; FQuat Q;
		const bool bL = PosField >= 0 && World->GetVector(Entity, Component, PosField, L);
		const bool bR = RotField >= 0 && World->GetQuat(Entity, Component, RotField, Q);
		if (bL && bR) O->SetActorLocationAndRotation(L, Q); else if (bL) O->SetActorLocation(L); else if (bR) O->SetActorRotation(Q);
	}
}
