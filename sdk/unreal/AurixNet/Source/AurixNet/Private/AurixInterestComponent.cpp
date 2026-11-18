#include "AurixInterestComponent.h"
#include "AurixNetSubsystem.h"
#include "AurixNetSettings.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"
#include "GameFramework/Actor.h"

UAurixInterestComponent::UAurixInterestComponent() { PrimaryComponentTick.bCanEverTick = true; }

UAurixNetSubsystem* UAurixInterestComponent::Net() const
{
	const UWorld* W = GetWorld(); const UGameInstance* GI = W ? W->GetGameInstance() : nullptr;
	return GI ? GI->GetSubsystem<UAurixNetSubsystem>() : nullptr;
}
float UAurixInterestComponent::Cell() const { return CellSize > 0.f ? CellSize : UAurixNetSettings::Get()->InterestCellSize; }

// Maps the 2D grid cell (X, Y) to a group in 1..65535. Cells wrap on a 255x255 torus, so far-apart positions can share a group; that only
// costs a few extra events, never drops one. Interest groups are per room and 0 means "everyone".
int32 UAurixInterestComponent::GroupFor(FVector L) const
{
	const float C = FMath::Max(1.f, Cell());
	const int32 X = FMath::FloorToInt(L.X / C), Y = FMath::FloorToInt(L.Y / C);
	const int32 Gx = ((X % 255) + 255) % 255, Gy = ((Y % 255) + 255) % 255;
	return 1 + Gx + Gy * 255;
}

void UAurixInterestComponent::BeginPlay() { Super::BeginPlay(); Recompute(true); }
void UAurixInterestComponent::EndPlay(const EEndPlayReason::Type Reason)
{
	if (UAurixNetSubsystem* N = Net()) { if (Joined.Num() && N->GetState() == EAurixNetState::InRoom) N->SetGroups({}, Joined); }
	Joined.Reset(); CurrentCell = 0;
	Super::EndPlay(Reason);
}
void UAurixInterestComponent::TickComponent(float Dt, ELevelTick T, FActorComponentTickFunction* F)
{
	Super::TickComponent(Dt, T, F);
	Accum += Dt;
	if (Accum >= UpdateInterval) { Accum = 0.f; Recompute(false); }
}

void UAurixInterestComponent::Recompute(bool bForce)
{
	UAurixNetSubsystem* N = Net();
	const AActor* O = GetOwner();
	if (!N || !O || N->GetState() != EAurixNetState::InRoom) return;
	const FVector L = O->GetActorLocation();
	const int32 NewCell = GroupFor(L);
	if (!bForce && NewCell == CurrentCell && Joined.Num()) return;
	const float C = FMath::Max(1.f, Cell());
	TArray<int32> Want;
	for (int32 Dy = -Radius; Dy <= Radius; ++Dy) for (int32 Dx = -Radius; Dx <= Radius; ++Dx) Want.AddUnique(GroupFor(L + FVector(Dx * C, Dy * C, 0)));
	TArray<int32> Join, Leave;
	for (int32 G : Want) if (!Joined.Contains(G)) Join.Add(G);
	for (int32 G : Joined) if (!Want.Contains(G)) Leave.Add(G);
	if (Join.Num() || Leave.Num()) N->SetGroups(Join, Leave);
	Joined = MoveTemp(Want);
	CurrentCell = NewCell;
}
