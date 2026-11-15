#include "AurixRollbackRunner.h"
#include "AurixNetLog.h"
#include "aurix_net.h"
#include "GameFramework/Actor.h"
#include "AurixNetSubsystem.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"

UAurixRollbackRunner::UAurixRollbackRunner() { PrimaryComponentTick.bCanEverTick = true; }

void UAurixRollbackRunner::BeginPlay()
{
	Super::BeginPlay();
	if (!Lockstep && GetOwner()) Lockstep = GetOwner()->FindComponentByClass<UAurixLockstepComponent>();
	if (Lockstep)
	{
		Lockstep->OnTick.AddDynamic(this, &UAurixRollbackRunner::HandleTick);
		Lockstep->OnRestore.AddDynamic(this, &UAurixRollbackRunner::HandleRestore);
		Lockstep->OnStateRequest.AddDynamic(this, &UAurixRollbackRunner::HandleStateRequest);
	}
	else UE_LOG(LogAurixNet, Warning, TEXT("AurixRollbackRunner: no AurixLockstepComponent on %s"), *GetNameSafe(GetOwner()));
}
void UAurixRollbackRunner::EndPlay(const EEndPlayReason::Type Reason)
{
	if (Lockstep)
	{
		Lockstep->OnTick.RemoveDynamic(this, &UAurixRollbackRunner::HandleTick);
		Lockstep->OnRestore.RemoveDynamic(this, &UAurixRollbackRunner::HandleRestore);
		Lockstep->OnStateRequest.RemoveDynamic(this, &UAurixRollbackRunner::HandleStateRequest);
	}
	Super::EndPlay(Reason);
}

void UAurixRollbackRunner::Start(int64 FromTick)
{
	Confirmed = Predicted = FromTick - 1;
	ConfirmedState = SaveState.IsBound() ? SaveState.Execute() : TArray<uint8>();
	Pending.Reset(); LastInputs.Reset(); Journal.Reset(); JournalStart = FromTick; RollbackCount = 0; Accum = 0;
	bRunning = true;
}
void UAurixRollbackRunner::Stop() { bRunning = false; }
void UAurixRollbackRunner::SetLocalInput(const TArray<uint8>& In) { LocalInput = In; }

int64 UAurixRollbackRunner::CurrentHash()
{
	if (HashState.IsBound()) return HashState.Execute();
	const TArray<uint8> S = SaveState.IsBound() ? SaveState.Execute() : TArray<uint8>();
	return (int64)aurix_net_lockstep_fnv1a(S.GetData(), S.Num());
}

void UAurixRollbackRunner::Simulate(int64 Tick, const TArray<FAurixLockstepInput>& Inputs, bool bPredicted) { OnSimulate.Broadcast(Tick, Inputs, bPredicted); }

int32 UAurixRollbackRunner::LocalActor() const
{
	const UWorld* W = GetWorld(); const UGameInstance* GI = W ? W->GetGameInstance() : nullptr;
	const UAurixNetSubsystem* N = GI ? GI->GetSubsystem<UAurixNetSubsystem>() : nullptr;
	return N ? N->GetRoom().Actor : -1;
}

// Prediction: every other player repeats their last confirmed input; we use the input we just sent.
TArray<FAurixLockstepInput> UAurixRollbackRunner::PredictedInputs() const
{
	TArray<FAurixLockstepInput> Out;
	const int32 Me = LocalActor();
	for (const auto& P : LastInputs) { if (P.Key == Me) continue; FAurixLockstepInput& I = Out.Emplace_GetRef(); I.Actor = P.Key; I.Data = P.Value; }
	FAurixLockstepInput& Mine = Out.Emplace_GetRef(); Mine.Actor = Me; Mine.Data = LocalInput;
	Out.Sort([](const FAurixLockstepInput& A, const FAurixLockstepInput& B) { return A.Actor < B.Actor; });
	return Out;
}

void UAurixRollbackRunner::TickComponent(float Dt, ELevelTick T, FActorComponentTickFunction* F)
{
	Super::TickComponent(Dt, T, F);
	if (!bRunning || !Lockstep) return;
	Accum += Dt;
	const double Step = 1.0 / FMath::Max(1, TickHz);
	while (Accum >= Step)
	{
		Accum -= Step;
		if (Predicted - Confirmed >= MaxPrediction) { Accum = FMath::Min(Accum, Step); break; } // stall: too far ahead of the cell
		const int64 Next = Predicted + 1;
		Lockstep->SendInput(LocalInput);
		Simulate(Next, PredictedInputs(), true);
		Predicted = Next;
	}
}

void UAurixRollbackRunner::HandleTick(int64 Tick, const TArray<FAurixLockstepInput>& Inputs)
{
	if (!bRunning) return;
	Pending.Add(Tick, Inputs);
	// Apply confirmed ticks in order from the confirmed state.
	if (!Pending.Contains(Confirmed + 1)) return;
	const int64 From = Confirmed + 1;
	const int64 WasPredicted = Predicted;
	if (LoadState.IsBound()) LoadState.Execute(ConfirmedState);
	int64 T = From;
	for (; Pending.Contains(T); ++T)
	{
		const TArray<FAurixLockstepInput>& In = Pending[T];
		for (const FAurixLockstepInput& I : In) LastInputs.Add(I.Actor, I.Data);
		Simulate(T, In, false);
		if (bRecordReplay) Journal.Emplace(T, In);
		Lockstep->ReportHash(T, CurrentHash());
		Pending.Remove(T);
	}
	Confirmed = T - 1;
	ConfirmedState = SaveState.IsBound() ? SaveState.Execute() : TArray<uint8>();
	// Re-simulate the ticks we had predicted beyond the new confirmed tick.
	int32 Resim = 0;
	for (int64 P = Confirmed + 1; P <= WasPredicted; ++P, ++Resim)
	{
		Simulate(P, PredictedInputs(), true);
	}
	Predicted = FMath::Max(Confirmed, WasPredicted);
	if (Resim > 0 || WasPredicted >= From) { ++RollbackCount; OnResimulated.Broadcast(From, Resim); }
}

void UAurixRollbackRunner::HandleRestore(int64 Tick, int64, const TArray<uint8>& State)
{
	// Late join / resync: the cell hands us a checkpoint; continue from it.
	if (LoadState.IsBound()) LoadState.Execute(State);
	ConfirmedState = State; Confirmed = Predicted = Tick; Pending.Reset();
	Journal.Reset(); JournalStart = Tick + 1;
	bRunning = true;
}
void UAurixRollbackRunner::HandleStateRequest(int64, bool bFullDump)
{
	if (!Lockstep) return;
	const int64 H = (int64)aurix_net_lockstep_fnv1a(ConfirmedState.GetData(), ConfirmedState.Num());
	if (bFullDump) Lockstep->SendDump(Confirmed, H, ConfirmedState); else Lockstep->SendCheckpoint(Confirmed, H, ConfirmedState);
}

TArray<uint8> UAurixRollbackRunner::ExportReplay() const
{
	TArray<uint8> Out;
	auto U16 = [&](uint16 V) { Out.Add(uint8(V >> 8)); Out.Add(uint8(V & 0xFF)); };
	auto U32 = [&](uint32 V) { U16(uint16(V >> 16)); U16(uint16(V & 0xFFFF)); };
	Out.Append((const uint8*)"AXRP", 4); U32((uint32)TickHz); U32((uint32)JournalStart);
	for (const auto& E : Journal)
	{
		U16((uint16)E.Value.Num());
		for (const FAurixLockstepInput& I : E.Value) { U16((uint16)I.Actor); U16((uint16)I.Data.Num()); Out.Append(I.Data); }
	}
	return Out;
}

bool UAurixRollbackRunner::PlayReplay(const TArray<uint8>& R, const TArray<uint8>& Initial)
{
	if (R.Num() < 12 || FMemory::Memcmp(R.GetData(), "AXRP", 4) != 0) return false;
	int32 O = 4;
	auto U16 = [&](uint16& V) { if (O + 2 > R.Num()) return false; V = uint16((R[O] << 8) | R[O + 1]); O += 2; return true; };
	auto U32 = [&](uint32& V) { uint16 Hi, Lo; if (!U16(Hi) || !U16(Lo)) return false; V = (uint32(Hi) << 16) | Lo; return true; };
	uint32 Hz, Start; if (!U32(Hz) || !U32(Start)) return false;
	bRunning = false;
	if (LoadState.IsBound()) LoadState.Execute(Initial);
	int64 T = Start;
	while (O < R.Num())
	{
		uint16 N; if (!U16(N)) return false;
		TArray<FAurixLockstepInput> In;
		for (uint16 I = 0; I < N; ++I)
		{
			uint16 A, L; if (!U16(A) || !U16(L) || O + L > R.Num()) return false;
			FAurixLockstepInput& X = In.Emplace_GetRef(); X.Actor = A; X.Data.Append(R.GetData() + O, L); O += L;
		}
		Simulate(T++, In, false);
	}
	return true;
}
