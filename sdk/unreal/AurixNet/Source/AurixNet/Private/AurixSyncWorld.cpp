#include "AurixSyncWorld.h"
#include "AurixNetSubsystem.h"
#include "AurixNetLog.h"
#include "aurix_net.h"
#include "Engine/GameInstance.h"
#include "Engine/World.h"

UAurixSyncWorld::UAurixSyncWorld() { PrimaryComponentTick.bCanEverTick = true; }

UAurixNetSubsystem* UAurixSyncWorld::Net() const
{
	const UWorld* W = GetWorld(); const UGameInstance* GI = W ? W->GetGameInstance() : nullptr;
	return GI ? GI->GetSubsystem<UAurixNetSubsystem>() : nullptr;
}

bool UAurixSyncWorld::Setup(const FString& InSchema)
{
	Teardown();
	SchemaJson = InSchema;
	S = aurix_net_sync_new(TCHAR_TO_UTF8(*SchemaJson));
	if (!S) { UE_LOG(LogAurixNet, Error, TEXT("sync schema rejected: %s"), *Net()->GetLastError()); return false; }
	if (UAurixNetSubsystem* N = Net()) DataHandle = N->OnRawData.AddUObject(this, &UAurixSyncWorld::OnRawData);
	return true;
}
void UAurixSyncWorld::BeginPlay() { Super::BeginPlay(); if (!SchemaJson.IsEmpty()) Setup(SchemaJson); }
void UAurixSyncWorld::EndPlay(const EEndPlayReason::Type Reason) { Teardown(); Super::EndPlay(Reason); }
void UAurixSyncWorld::Teardown()
{
	if (UAurixNetSubsystem* N = Net()) { if (DataHandle.IsValid()) { N->OnRawData.Remove(DataHandle); DataHandle.Reset(); } }
	if (S) { aurix_net_sync_destroy(S); S = nullptr; }
}

void UAurixSyncWorld::OnRawData(uint8, TArrayView<const uint8> Data)
{
	if (!S) return;
	uint8 Ack[9];
	if (aurix_net_sync_receive(S, Data.GetData(), Data.Num(), Ack) == 1)
	{
		if (UAurixNetSubsystem* N = Net()) N->SendData(EAurixNetChannel::Unreliable, TArrayView<const uint8>(Ack, 9));
	}
}

void UAurixSyncWorld::TickComponent(float DeltaTime, ELevelTick TickType, FActorComponentTickFunction* F)
{
	Super::TickComponent(DeltaTime, TickType, F);
	if (!S) return;
	AurixNetSyncEvent Ev;
	while (aurix_net_sync_poll(S, &Ev) == 1)
	{
		switch (Ev.kind)
		{
		case AURIX_NET_SYNC_SPAWN: OnSpawn.Broadcast((int64)Ev.entity, Ev.owner); break;
		case AURIX_NET_SYNC_CHANGE: OnChange.Broadcast((int64)Ev.entity, Ev.owner); break;
		case AURIX_NET_SYNC_DESPAWN: OnDespawn.Broadcast((int64)Ev.entity, Ev.owner); break;
		default: break;
		}
	}
}

int64 UAurixSyncWorld::ServerTick() const { return S ? (int64)aurix_net_sync_server_tick(S) : -1; }
int64 UAurixSyncWorld::Gaps() const { return S ? (int64)aurix_net_sync_gaps(S) : 0; }
int32 UAurixSyncWorld::EntityCount() const { return S ? aurix_net_sync_entity_count(S) : 0; }
bool UAurixSyncWorld::EntityAt(int32 Index, int64& Entity, int32& Owner) const
{
	uint64_t Id = 0; int32_t O = -1;
	if (!S || aurix_net_sync_entity_at(S, Index, &Id, &O) != 1) return false;
	Entity = (int64)Id; Owner = O; return true;
}
int32 UAurixSyncWorld::ComponentId(const FString& Name) const { return S ? aurix_net_sync_component(S, TCHAR_TO_UTF8(*Name)) : -1; }
int32 UAurixSyncWorld::FieldIndex(int32 C, const FString& Name) const { return S ? aurix_net_sync_field(S, C, TCHAR_TO_UTF8(*Name)) : -1; }
bool UAurixSyncWorld::Has(int64 E, int32 C) const { return S && aurix_net_sync_has(S, (uint64)E, C) == 1; }
bool UAurixSyncWorld::GetNumber(int64 E, int32 C, int32 F, double& Out) const { return S && aurix_net_sync_get_number(S, (uint64)E, C, F, &Out) == 1; }
bool UAurixSyncWorld::GetVector(int64 E, int32 C, int32 F, FVector& Out) const
{
	float V[4] = { 0, 0, 0, 0 };
	if (!S || aurix_net_sync_get_vector(S, (uint64)E, C, F, V) < 2) return false;
	Out = FVector(V[0], V[1], V[2]); return true;
}
bool UAurixSyncWorld::GetQuat(int64 E, int32 C, int32 F, FQuat& Out) const
{
	float V[4] = { 0, 0, 0, 1 };
	if (!S || aurix_net_sync_get_vector(S, (uint64)E, C, F, V) != 4) return false;
	Out = FQuat(V[0], V[1], V[2], V[3]); return true;
}
bool UAurixSyncWorld::GetBytes(int64 E, int32 C, int32 F, TArray<uint8>& Out) const
{
	if (!S) return false;
	const int32 Len = aurix_net_sync_get_bytes(S, (uint64)E, C, F, nullptr, 0);
	if (Len < 0) return false;
	Out.SetNumUninitialized(Len);
	if (Len > 0) aurix_net_sync_get_bytes(S, (uint64)E, C, F, Out.GetData(), Len);
	return true;
}
bool UAurixSyncWorld::GetString(int64 E, int32 C, int32 F, FString& Out) const
{
	TArray<uint8> B; if (!GetBytes(E, C, F, B)) return false;
	B.Add(0); Out = UTF8_TO_TCHAR(reinterpret_cast<const ANSICHAR*>(B.GetData())); return true;
}

int64 UAurixSyncWorld::SendInput(const TArray<uint8>& Data)
{
	UAurixNetSubsystem* N = Net();
	return (S && N && N->NativeClient()) ? aurix_net_sync_send_input(S, N->NativeClient(), Data.GetData(), Data.Num()) : -1;
}
bool UAurixSyncWorld::WriteBegin(int32 C) { return S && aurix_net_sync_write_begin(S, C) == 0; }
bool UAurixSyncWorld::WriteNumber(int32 F, double V) { return S && aurix_net_sync_write_number(S, F, V) == 0; }
bool UAurixSyncWorld::WriteVector(int32 F, FVector V) { const float A[3] = { (float)V.X, (float)V.Y, (float)V.Z }; return S && aurix_net_sync_write_vector(S, F, A, 3) == 0; }
bool UAurixSyncWorld::WriteQuat(int32 F, FQuat Q) { const float A[4] = { (float)Q.X, (float)Q.Y, (float)Q.Z, (float)Q.W }; return S && aurix_net_sync_write_vector(S, F, A, 4) == 0; }
bool UAurixSyncWorld::WriteBytes(int32 F, const TArray<uint8>& V) { return S && aurix_net_sync_write_bytes(S, F, V.GetData(), V.Num()) == 0; }
bool UAurixSyncWorld::WriteEnd() { return S && aurix_net_sync_write_end(S) == 0; }
int64 UAurixSyncWorld::SendWrites() { UAurixNetSubsystem* N = Net(); return (S && N && N->NativeClient()) ? aurix_net_sync_send_writes(S, N->NativeClient()) : -1; }
int64 UAurixSyncWorld::SendOp(int32 Op, int64 E, int32 To) { UAurixNetSubsystem* N = Net(); return (S && N && N->NativeClient()) ? aurix_net_sync_send_op(S, N->NativeClient(), Op, (uint64)E, To) : -1; }
