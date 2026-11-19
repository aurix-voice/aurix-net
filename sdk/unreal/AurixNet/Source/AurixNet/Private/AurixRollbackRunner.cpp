#include "AurixRollbackRunner.h"
#include "AurixNetLog.h"
#include "aurix_net.h"
#include "GameFramework/Actor.h"
#include "AurixNetSubsystem.h"
#include "Engine/World.h"

UAurixRollbackRunner::UAurixRollbackRunner() { PrimaryComponentTick.bCanEverTick = true; }

void UAurixRollbackRunner::BeginPlay()
{
	Super::BeginPlay();
	if (!Lockstep && GetOwner()) Lockstep = GetOwner()->FindComponentByClass<UAurixLockstepComponent>();
	if (Lockstep)
	{
		Lockstep->OnCommand.AddDynamic(this, &UAurixRollbackRunner::HandleCommand);
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
		Lockstep->OnCommand.RemoveDynamic(this, &UAurixRollbackRunner::HandleCommand);
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
	InitialState = ConfirmedState;
	Arriving.Reset(); Pending.Reset(); LastInputs.Reset(); Journal.Reset(); JournalStart = FromTick; RollbackCount = 0; Accum = 0;
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

void UAurixRollbackRunner::Simulate(int64 Tick, const TArray<FAurixLockstepInput>& Inputs, const TArray<FAurixTickCommand>& Commands, bool bPredicted) { OnSimulate.Broadcast(Tick, Inputs, Commands, bPredicted); }

int32 UAurixRollbackRunner::LocalActor() const
{
	const UAurixNetSubsystem* N = UAurixNetSubsystem::Get(this);
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
		Simulate(Next, PredictedInputs(), TArray<FAurixTickCommand>(), true);
		Predicted = Next;
	}
}

void UAurixRollbackRunner::HandleCommand(int64, int32 Actor, const TArray<uint8>& Data)
{
	FAurixTickCommand& C = Arriving.Emplace_GetRef(); C.Actor = Actor; C.Data = Data;
}

void UAurixRollbackRunner::HandleTick(int64 Tick, const TArray<FAurixLockstepInput>& Inputs)
{
	TArray<FAurixTickCommand> Commands = MoveTemp(Arriving);
	if (!bRunning) return;
	FConfirmedTick& Entry = Pending.Add(Tick);
	Entry.Inputs = Inputs; Entry.Commands = MoveTemp(Commands);
	// Apply confirmed ticks in order from the confirmed state.
	if (!Pending.Contains(Confirmed + 1)) return;
	const int64 From = Confirmed + 1;
	const int64 WasPredicted = Predicted;
	if (LoadState.IsBound()) LoadState.Execute(ConfirmedState);
	int64 T = From;
	for (; Pending.Contains(T); ++T)
	{
		FConfirmedTick In = Pending.FindAndRemoveChecked(T);
		for (const FAurixLockstepInput& I : In.Inputs) LastInputs.Add(I.Actor, I.Data);
		Simulate(T, In.Inputs, In.Commands, false);
		Lockstep->ReportHash(T, CurrentHash());
		if (bRecordReplay) Journal.Add(MoveTemp(In));
	}
	Confirmed = T - 1;
	ConfirmedState = SaveState.IsBound() ? SaveState.Execute() : TArray<uint8>();
	// Re-simulate the ticks we had predicted beyond the new confirmed tick.
	int32 Resim = 0;
	for (int64 P = Confirmed + 1; P <= WasPredicted; ++P, ++Resim)
	{
		Simulate(P, PredictedInputs(), TArray<FAurixTickCommand>(), true);
	}
	Predicted = FMath::Max(Confirmed, WasPredicted);
	if (Resim > 0 || WasPredicted >= From) { ++RollbackCount; OnResimulated.Broadcast(From, Resim); }
}

void UAurixRollbackRunner::HandleRestore(int64 Tick, int64, const TArray<uint8>& State)
{
	// Late join / resync: the cell hands us the state before Tick, or nothing when the room has no checkpoint yet.
	ConfirmedState = State.IsEmpty() ? InitialState : State;
	if (LoadState.IsBound()) LoadState.Execute(ConfirmedState);
	Confirmed = Predicted = Tick - 1; Arriving.Reset(); Pending.Reset();
	Journal.Reset(); JournalStart = Tick;
	bRunning = true;
}
void UAurixRollbackRunner::HandleStateRequest(int64, bool bFullDump)
{
	if (!Lockstep) return;
	const int64 H = (int64)aurix_net_lockstep_fnv1a(ConfirmedState.GetData(), ConfirmedState.Num());
	if (bFullDump) Lockstep->SendDump(Confirmed, H, ConfirmedState); else Lockstep->SendCheckpoint(Confirmed + 1, H, ConfirmedState);
}

TArray<uint8> UAurixRollbackRunner::ExportReplay() const
{
	TArray<uint8> Out;
	auto U16 = [&](uint16 V) { Out.Add(uint8(V >> 8)); Out.Add(uint8(V & 0xFF)); };
	auto U32 = [&](uint32 V) { U16(uint16(V >> 16)); U16(uint16(V & 0xFFFF)); };
	Out.Append((const uint8*)"AXRP", 4); U32((uint32)TickHz); U32((uint32)JournalStart);
	for (const FConfirmedTick& E : Journal)
	{
		U16((uint16)(E.Inputs.Num() + (E.Commands.IsEmpty() ? 0 : 1)));
		for (const FAurixLockstepInput& I : E.Inputs) { U16((uint16)I.Actor); U16((uint16)I.Data.Num()); Out.Append(I.Data); }
		if (E.Commands.IsEmpty()) continue;
		int32 Len = 0;
		for (const FAurixTickCommand& C : E.Commands) Len += 4 + C.Data.Num();
		U16(0xFFFF); U16((uint16)Len);
		for (const FAurixTickCommand& C : E.Commands) { U16((uint16)C.Actor); U16((uint16)C.Data.Num()); Out.Append(C.Data); }
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
		TArray<FAurixTickCommand> Commands;
		for (uint16 I = 0; I < N; ++I)
		{
			uint16 A, L; if (!U16(A) || !U16(L) || O + L > R.Num()) return false;
			if (A == 0xFFFF) Commands = UAurixLockstepComponent::ReadCommands(TArray<uint8>(R.GetData() + O, L));
			else { FAurixLockstepInput& X = In.Emplace_GetRef(); X.Actor = A; X.Data.Append(R.GetData() + O, L); }
			O += L;
		}
		Simulate(T++, In, Commands, false);
	}
	return true;
}
