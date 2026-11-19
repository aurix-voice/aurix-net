#include "AurixPhysicsPredictionComponent.h"
#include "AurixNetSubsystem.h"
#include "AurixNetLog.h"
#include "Components/PrimitiveComponent.h"
#include "GameFramework/Actor.h"
#include "Engine/World.h"
#include "Misc/Crc.h"
#include "Runtime/Launch/Resources/Version.h"

UAurixPhysicsPredictionComponent::UAurixPhysicsPredictionComponent() { PrimaryComponentTick.bCanEverTick = true; PrimaryComponentTick.TickGroup = TG_PostPhysics; }

UPrimitiveComponent* UAurixPhysicsPredictionComponent::Body() const { const AActor* O = GetOwner(); return O ? Cast<UPrimitiveComponent>(O->GetRootComponent()) : nullptr; }
UAurixNetSubsystem* UAurixPhysicsPredictionComponent::Net() const
{
	return UAurixNetSubsystem::Get(this);
}
uint32 UAurixPhysicsPredictionComponent::EffectiveId() const { return BodyId > 0 ? (uint32)BodyId : FCrc::StrCrc32(*GetNameSafe(GetOwner())); }

void UAurixPhysicsPredictionComponent::ApplyEngineMode()
{
	AActor* O = GetOwner();
	if (!O || !Body()) return;
#if ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION >= 5
	O->SetPhysicsReplicationMode(bPreferResimulation ? EPhysicsReplicationMode::Resimulation : EPhysicsReplicationMode::PredictiveInterpolation);
#elif ENGINE_MAJOR_VERSION == 5 && ENGINE_MINOR_VERSION == 4
	O->SetPhysicsReplicationMode(EPhysicsReplicationMode::PredictiveInterpolation);
#else
	// 5.3: no per-body replication mode; the engine's default physics replication applies.
	UE_LOG(LogAurixNet, Verbose, TEXT("physics prediction modes need UE 5.4+; using default replication for %s"), *GetNameSafe(GetOwner()));
#endif
}

void UAurixPhysicsPredictionComponent::BeginPlay()
{
	Super::BeginPlay();
	if (Mode == EAurixPhysicsMode::EngineReplication) { ApplyEngineMode(); return; }
	if (UAurixNetSubsystem* N = Net()) RawHandle = N->OnRawEvent.AddUObject(this, &UAurixPhysicsPredictionComponent::OnRawEvent);
	if (UPrimitiveComponent* P = Body()) { if (!bOwner) P->SetSimulatePhysics(false); }
}
void UAurixPhysicsPredictionComponent::EndPlay(const EEndPlayReason::Type Reason)
{
	if (UAurixNetSubsystem* N = Net()) { if (RawHandle.IsValid()) N->OnRawEvent.Remove(RawHandle); }
	Super::EndPlay(Reason);
}

// Snapshot: u32 id, f32 pos[3], f32 rot[4], f32 vel[3], f32 angvel[3] = 56 bytes.
void UAurixPhysicsPredictionComponent::SendSnapshot()
{
	UPrimitiveComponent* P = Body(); UAurixNetSubsystem* N = Net();
	if (!P || !N || N->GetState() != EAurixNetState::InRoom) return;
	TArray<uint8> B; B.Reserve(56);
	auto F32 = [&](float V) { B.Append(reinterpret_cast<const uint8*>(&V), 4); };
	const uint32 Id = EffectiveId(); B.Append(reinterpret_cast<const uint8*>(&Id), 4);
	const FVector L = P->GetComponentLocation(); const FQuat R = P->GetComponentQuat();
	const FVector V = P->GetPhysicsLinearVelocity(); const FVector A = P->GetPhysicsAngularVelocityInRadians();
	F32(L.X); F32(L.Y); F32(L.Z); F32(R.X); F32(R.Y); F32(R.Z); F32(R.W); F32(V.X); F32(V.Y); F32(V.Z); F32(A.X); F32(A.Y); F32(A.Z);
	N->SendEventEx(SnapshotCode, B, EAurixNetTarget::Others, {}, EAurixNetChannel::Unreliable, Group, EAurixNetCache::None);
}

void UAurixPhysicsPredictionComponent::OnRawEvent(const FAurixNetEventFrame& F)
{
	if (F.Code != SnapshotCode || F.Payload.Num() < 56 || bOwner) return;
	const uint8* D = F.Payload.GetData();
	uint32 Id; FMemory::Memcpy(&Id, D, 4);
	if (Id != EffectiveId()) return;
	float V[13]; FMemory::Memcpy(V, D + 4, 52);
	UPrimitiveComponent* P = Body(); if (!P) return;
	const FVector NewPos(V[0], V[1], V[2]); const FQuat NewRot(V[3], V[4], V[5], V[6]);
	TargetVel = FVector(V[7], V[8], V[9]); TargetAngVel = FVector(V[10], V[11], V[12]);
	// Error between where we show the body and where the owner says it is; blended away over CorrectionTime.
	const FVector Cur = P->GetComponentLocation(); const FQuat CurRot = P->GetComponentQuat();
	PosError = Cur - NewPos; RotError = CurRot * NewRot.Inverse();
	LastError = PosError.Size();
	if (CorrectionTime <= 0.f || LastError > SnapDistance) { PosError = FVector::ZeroVector; RotError = FQuat::Identity; }
	TargetPos = NewPos; TargetRot = NewRot.GetNormalized(); Blend = 1.f; bHaveTarget = true;
}

void UAurixPhysicsPredictionComponent::TickComponent(float Dt, ELevelTick T, FActorComponentTickFunction* Fn)
{
	Super::TickComponent(Dt, T, Fn);
	if (Mode != EAurixPhysicsMode::RelayEvents) return;
	if (bOwner)
	{
		Accum += Dt;
		const float Step = 1.f / FMath::Max(1, SendHz);
		if (Accum >= Step) { Accum = FMath::Fmod(Accum, Step); SendSnapshot(); }
		return;
	}
	UPrimitiveComponent* P = Body(); if (!P || !bHaveTarget) return;
	// Extrapolate the authoritative state with its velocities, then add the decaying visual error.
	TargetPos += TargetVel * Dt;
	const FVector Axis = TargetAngVel.GetSafeNormal(); const float Ang = TargetAngVel.Size() * Dt;
	if (Ang > KINDA_SMALL_NUMBER) TargetRot = (FQuat(Axis, Ang) * TargetRot).GetNormalized();
	if (CorrectionTime > 0.f && Blend > 0.f) Blend = FMath::Max(0.f, Blend - Dt / CorrectionTime);
	const FVector ShowPos = TargetPos + PosError * Blend;
	const FQuat ShowRot = FQuat::Slerp(TargetRot, RotError * TargetRot, Blend).GetNormalized();
	P->SetWorldLocationAndRotation(ShowPos, ShowRot, false, nullptr, ETeleportType::TeleportPhysics);
}
