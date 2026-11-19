#include "AurixLockstepComponent.h"
#include "AurixNetSubsystem.h"
#include "AurixNetLog.h"
#include "aurix_net.h"
#include "Engine/World.h"

UAurixLockstepComponent::UAurixLockstepComponent() { PrimaryComponentTick.bCanEverTick = true; }

UAurixNetSubsystem* UAurixLockstepComponent::Net() const
{
	return UAurixNetSubsystem::Get(this);
}

void UAurixLockstepComponent::BeginPlay()
{
	Super::BeginPlay();
	Ls = aurix_net_lockstep_new();
	if (UAurixNetSubsystem* N = Net()) DataHandle = N->OnRawData.AddUObject(this, &UAurixLockstepComponent::OnRawData);
}
void UAurixLockstepComponent::EndPlay(const EEndPlayReason::Type Reason)
{
	if (UAurixNetSubsystem* N = Net()) { if (DataHandle.IsValid()) N->OnRawData.Remove(DataHandle); }
	if (Ls) { aurix_net_lockstep_destroy(Ls); Ls = nullptr; }
	Super::EndPlay(Reason);
}

void UAurixLockstepComponent::OnRawData(uint8, TArrayView<const uint8> Data)
{
	UAurixNetSubsystem* N = Net();
	if (Ls && N) aurix_net_lockstep_receive(Ls, N->NativeClient(), Data.GetData(), Data.Num());
}

void UAurixLockstepComponent::TickComponent(float DeltaTime, ELevelTick TickType, FActorComponentTickFunction* F)
{
	Super::TickComponent(DeltaTime, TickType, F);
	if (!Ls) return;
	AurixNetLsEvent Ev;
	while (aurix_net_lockstep_poll(Ls, &Ev) == 1)
	{
		switch (Ev.kind)
		{
		case AURIX_NET_LS_TICK:
		{
			TArray<FAurixLockstepInput> Inputs; Inputs.Reserve(Ev.inputs);
			for (uint16 I = 0; I < Ev.inputs; ++I)
			{
				uint16 Actor = 0; const uint8* D = nullptr; size_t L = 0;
				if (aurix_net_lockstep_input(Ls, I, &Actor, &D, &L) == 1)
				{
					FAurixLockstepInput& In = Inputs.Emplace_GetRef(); In.Actor = Actor; In.Data.Append(D, (int32)L);
				}
			}
			for (int32 I = Inputs.Num() - 1; I >= 0; --I)
			{
				if (Inputs[I].Actor != 0xFFFF) continue;
				const TArray<uint8>& C = Inputs[I].Data;
				for (int32 P = 0; P + 4 <= C.Num();)
				{
					const int32 From = (C[P] << 8) | C[P + 1]; const int32 Len = (C[P + 2] << 8) | C[P + 3]; P += 4;
					if (P + Len > C.Num()) break;
					OnCommand.Broadcast((int64)Ev.tick, From, TArray<uint8>(C.GetData() + P, Len)); P += Len;
				}
				Inputs.RemoveAt(I);
			}
			OnTick.Broadcast((int64)Ev.tick, Inputs);
			break;
		}
		case AURIX_NET_LS_RESTORE:
		{
			TArray<uint8> State; State.Append(Ev.state, (int32)Ev.state_len);
			OnRestore.Broadcast((int64)Ev.tick, (int64)Ev.hash, State);
			break;
		}
		case AURIX_NET_LS_CHECKPOINT_REQ: OnStateRequest.Broadcast((int64)Ev.tick, false); break;
		case AURIX_NET_LS_DUMP_REQ: OnStateRequest.Broadcast((int64)Ev.tick, true); break;
		default: break;
		}
	}
}

int64 UAurixLockstepComponent::SendInput(const TArray<uint8>& Data)
{
	UAurixNetSubsystem* N = Net();
	if (!Ls || !N || !N->NativeClient()) return -1;
	return aurix_net_lockstep_send_input(Ls, N->NativeClient(), Data.GetData(), Data.Num());
}
bool UAurixLockstepComponent::SendCommand(const TArray<uint8>& Data)
{
	UAurixNetSubsystem* N = Net();
	return N && N->NativeClient() && Data.Num() > 0 && Data.Num() <= 1024 && aurix_net_lockstep_send_command(N->NativeClient(), Data.GetData(), Data.Num()) == 0;
}
bool UAurixLockstepComponent::ReportHash(int64 Tick, int64 Hash)
{
	UAurixNetSubsystem* N = Net();
	return N && N->NativeClient() && aurix_net_lockstep_report_hash(N->NativeClient(), (uint32)Tick, (uint64)Hash) == 0;
}
bool UAurixLockstepComponent::SendCheckpoint(int64 Tick, int64 Hash, const TArray<uint8>& State)
{
	UAurixNetSubsystem* N = Net();
	return N && N->NativeClient() && aurix_net_lockstep_send_checkpoint(N->NativeClient(), (uint32)Tick, (uint64)Hash, State.GetData(), State.Num()) == 0;
}
bool UAurixLockstepComponent::SendDump(int64 Tick, int64 Hash, const TArray<uint8>& State)
{
	UAurixNetSubsystem* N = Net();
	return N && N->NativeClient() && aurix_net_lockstep_send_dump(N->NativeClient(), (uint32)Tick, (uint64)Hash, State.GetData(), State.Num()) == 0;
}
int64 UAurixLockstepComponent::HashBytes(const TArray<uint8>& Data) { return (int64)aurix_net_lockstep_fnv1a(Data.GetData(), Data.Num()); }
int64 UAurixLockstepComponent::ConfirmedTick() const { return GetStat(AURIX_NET_LS_CONFIRMED_TICK); }
int64 UAurixLockstepComponent::GetStat(int32 Key) const { return Ls ? aurix_net_lockstep_get(Ls, Key) : -1; }
bool UAurixLockstepComponent::SetOption(int32 Key, int64 Value) { return Ls && aurix_net_lockstep_set(Ls, Key, Value) == 0; }
