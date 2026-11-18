#include "AurixSimWorld.h"
#include "AurixNetLog.h"
#include "UObject/Package.h"
#include "aurix_sim.h"

static int64 F(double V) { return UAurixFixedPoint::Fx(V); }
static double D(int64 V) { return UAurixFixedPoint::Dx(V); }

// ---------------------------------------------------------------- 2D
UAurixSimWorld2D* UAurixSimWorld2D::CreateWorld2D(UObject* Outer, FVector2D G, int32 TickHz)
{
	UAurixSimWorld2D* W = NewObject<UAurixSimWorld2D>(Outer ? Outer : (UObject*)GetTransientPackage());
	W->Handle = sim_world_new(F(G.X), F(G.Y), (uint32)FMath::Clamp(TickHz, 1, 1000));
	return W;
}
void UAurixSimWorld2D::BeginDestroy() { if (Handle) { sim_world_free(Handle); Handle = 0; } Super::BeginDestroy(); }
void UAurixSimWorld2D::Step() { if (Handle) sim_step(Handle); }
int64 UAurixSimWorld2D::Tick() const { return Handle ? (int64)sim_tick(Handle) : -1; }
int64 UAurixSimWorld2D::Hash() const { return Handle ? (int64)sim_hash(Handle) : 0; }

TArray<uint8> UAurixSimWorld2D::ReadBuffer(uint32 Len) const
{
	TArray<uint8> Out; Out.SetNumUninitialized((int32)Len);
	for (uint32 I = 0; I < Len; ++I) Out[I] = (uint8)sim_snapshot_byte(Handle, I);
	return Out;
}
void UAurixSimWorld2D::LoadBuffer(const TArray<uint8>& B) const
{
	sim_snapshot_set_len(Handle, (uint32)B.Num());
	for (int32 I = 0; I < B.Num(); ++I) sim_snapshot_set_byte(Handle, (uint32)I, B[I]);
}
TArray<uint8> UAurixSimWorld2D::Snapshot() const { return Handle ? ReadBuffer(sim_snapshot(Handle)) : TArray<uint8>(); }
bool UAurixSimWorld2D::Restore(const TArray<uint8>& S) { if (!Handle) return false; LoadBuffer(S); return sim_restore(Handle) == 1; }

int32 UAurixSimWorld2D::AddCircle(EAurixSimBodyKind K, FVector2D P, double R, double Den) { return Handle ? (int32)sim_add_circle(Handle, (uint32)K, F(P.X), F(P.Y), F(R), F(Den)) : -1; }
int32 UAurixSimWorld2D::AddBox(EAurixSimBodyKind K, FVector2D P, FVector2D H, double Den) { return Handle ? (int32)sim_add_box(Handle, (uint32)K, F(P.X), F(P.Y), F(H.X), F(H.Y), F(Den)) : -1; }
int32 UAurixSimWorld2D::AddCapsule(EAurixSimBodyKind K, FVector2D P, double HL, double R, double Den) { return Handle ? (int32)sim_add_capsule(Handle, (uint32)K, F(P.X), F(P.Y), F(HL), F(R), F(Den)) : -1; }
void UAurixSimWorld2D::RemoveBody(int32 B) { if (Handle) sim_remove_body(Handle, (uint32)B); }
void UAurixSimWorld2D::SetMaterial(int32 B, double Fr, double Re) { if (Handle) sim_set_material(Handle, (uint32)B, F(Fr), F(Re)); }
void UAurixSimWorld2D::SetLayers(int32 B, int32 L, int32 M, bool bS) { if (Handle) sim_set_layers(Handle, (uint32)B, (uint32)L, (uint32)M, bS ? 1 : 0); }
void UAurixSimWorld2D::SetVelocity(int32 B, FVector2D V, double W) { if (Handle) sim_set_velocity(Handle, (uint32)B, F(V.X), F(V.Y), F(W)); }
void UAurixSimWorld2D::SetPosition(int32 B, FVector2D P, double A) { if (Handle) sim_set_position(Handle, (uint32)B, F(P.X), F(P.Y), F(A)); }
void UAurixSimWorld2D::ApplyImpulse(int32 B, FVector2D I) { if (Handle) sim_apply_impulse(Handle, (uint32)B, F(I.X), F(I.Y)); }
int64 UAurixSimWorld2D::GetBodyRaw(int32 B, int32 Field) const { return Handle ? sim_body_get(Handle, (uint32)B, (uint32)Field) : 0; }
FVector2D UAurixSimWorld2D::GetPosition(int32 B) const { return FVector2D(D(GetBodyRaw(B, 0)), D(GetBodyRaw(B, 1))); }
double UAurixSimWorld2D::GetAngle(int32 B) const { return D(GetBodyRaw(B, 2)); }
FVector2D UAurixSimWorld2D::GetVelocity(int32 B) const { return FVector2D(D(GetBodyRaw(B, 3)), D(GetBodyRaw(B, 4))); }

bool UAurixSimWorld2D::ReadHit(uint32 R, FAurixSimHit& Hit) const
{
	if (R == 0) return false;
	Hit.Body = (int32)R; Hit.Fraction = D(sim_ray_get(0));
	Hit.Point = FVector(D(sim_ray_get(1)), D(sim_ray_get(2)), 0); Hit.Normal = FVector(D(sim_ray_get(3)), D(sim_ray_get(4)), 0);
	return true;
}
bool UAurixSimWorld2D::Raycast(FVector2D O, FVector2D Dir, int32 Mask, FAurixSimHit& Hit) const { return Handle && ReadHit(sim_raycast(Handle, F(O.X), F(O.Y), F(Dir.X), F(Dir.Y), (uint32)Mask), Hit); }
bool UAurixSimWorld2D::CircleCast(FVector2D O, FVector2D Dir, double R, int32 Mask, FAurixSimHit& Hit) const { return Handle && ReadHit(sim_circlecast(Handle, F(O.X), F(O.Y), F(Dir.X), F(Dir.Y), F(R), (uint32)Mask), Hit); }
TArray<FAurixSimContact> UAurixSimWorld2D::Contacts() const
{
	TArray<FAurixSimContact> Out;
	if (!Handle) return Out;
	const uint32 N = sim_events_len(Handle);
	for (uint32 I = 0; I < N; ++I) { FAurixSimContact& C = Out.Emplace_GetRef(); C.BodyA = (int32)sim_event_get(Handle, I, 0); C.BodyB = (int32)sim_event_get(Handle, I, 1); C.bBegin = sim_event_get(Handle, I, 2) == 1; }
	return Out;
}
int32 UAurixSimWorld2D::CreateCharacter(int32 Body, double StepH, double MinNy) { return Handle ? (int32)sim_kcc_new(Handle, (uint32)Body, F(StepH), F(MinNy)) : -1; }
bool UAurixSimWorld2D::MoveCharacter(int32 K, FVector2D Dl) { return Handle && sim_kcc_move(Handle, (uint32)K, F(Dl.X), F(Dl.Y)) == 1; }
int32 UAurixSimWorld2D::JointDistance(int32 A, int32 B, FVector2D AA, FVector2D AB, double Len, double MinLen) { return Handle ? (int32)sim_joint_distance(Handle, (uint32)A, (uint32)B, F(AA.X), F(AA.Y), F(AB.X), F(AB.Y), F(Len), F(MinLen)) : -1; }
int32 UAurixSimWorld2D::JointRevolute(int32 A, int32 B, FVector2D AA, FVector2D AB, bool bL, double Lo, double Up, double Sp, double Tq) { return Handle ? (int32)sim_joint_revolute(Handle, (uint32)A, (uint32)B, F(AA.X), F(AA.Y), F(AB.X), F(AB.Y), bL ? 1 : 0, F(Lo), F(Up), F(Sp), F(Tq)) : -1; }
int32 UAurixSimWorld2D::JointWeld(int32 A, int32 B, FVector2D AA, FVector2D AB, double Ang) { return Handle ? (int32)sim_joint_weld(Handle, (uint32)A, (uint32)B, F(AA.X), F(AA.Y), F(AB.X), F(AB.Y), F(Ang)) : -1; }
void UAurixSimWorld2D::JointMotor(int32 J, double Sp, double Tq) { if (Handle) sim_joint_motor(Handle, (uint32)J, F(Sp), F(Tq)); }
void UAurixSimWorld2D::RemoveJoint(int32 J) { if (Handle) sim_joint_remove(Handle, (uint32)J); }

bool UAurixSimWorld2D::NavGrid(FVector2D O, double Cell, int32 Cols, int32 Rows, const TArray<FIntPoint>& Blocked)
{
	if (!Handle) return false;
	TArray<uint8> B; B.Reserve(Blocked.Num() * 8);
	for (const FIntPoint& P : Blocked) { const uint32 C = (uint32)P.X, R = (uint32)P.Y; B.Append(reinterpret_cast<const uint8*>(&C), 4); B.Append(reinterpret_cast<const uint8*>(&R), 4); }
	LoadBuffer(B);
	return sim_nav_grid(Handle, F(O.X), F(O.Y), F(Cell), (uint32)Cols, (uint32)Rows) == 1;
}
bool UAurixSimWorld2D::NavLoad(const TArray<uint8>& Json) { if (!Handle) return false; LoadBuffer(Json); return sim_nav_load(Handle) == 1; }
int64 UAurixSimWorld2D::NavHash() const { return Handle ? (int64)sim_nav_hash(Handle) : 0; }
TArray<FVector2D> UAurixSimWorld2D::NavPath(FVector2D A, FVector2D B, int32 Mask)
{
	TArray<FVector2D> Out;
	if (!Handle) return Out;
	const uint32 N = sim_nav_path(Handle, F(A.X), F(A.Y), F(B.X), F(B.Y), (uint32)Mask);
	for (uint32 I = 0; I < N; ++I) Out.Emplace(D(sim_nav_point(Handle, I * 2)), D(sim_nav_point(Handle, I * 2 + 1)));
	return Out;
}
bool UAurixSimWorld2D::NavRaycast(FVector2D A, FVector2D B, FVector2D& Hit) const
{
	if (!Handle || sim_nav_raycast(Handle, F(A.X), F(A.Y), F(B.X), F(B.Y)) != 1) return false;
	Hit = FVector2D(D(sim_ray_get(1)), D(sim_ray_get(2))); return true;
}
int32 UAurixSimWorld2D::NavFlowField(FVector2D To, int32 Mask) { if (!Handle) return -1; const uint32 R = sim_nav_field(Handle, F(To.X), F(To.Y), (uint32)Mask); return R == 0xFFFFFFFFu ? -1 : (int32)R; }
int32 UAurixSimWorld2D::AgentAdd(FVector2D P, double R, double S) { return Handle ? (int32)sim_agent_add(Handle, F(P.X), F(P.Y), F(R), F(S)) : -1; }
void UAurixSimWorld2D::AgentRemove(int32 A) { if (Handle) sim_agent_remove(Handle, (uint32)A); }
void UAurixSimWorld2D::AgentTarget(int32 A, FVector2D To) { if (Handle) sim_agent_target(Handle, (uint32)A, F(To.X), F(To.Y)); }
void UAurixSimWorld2D::AgentFollow(int32 A, int32 Fld) { if (Handle) sim_agent_follow(Handle, (uint32)A, (uint32)Fld); }
void UAurixSimWorld2D::AgentStop(int32 A) { if (Handle) sim_agent_stop(Handle, (uint32)A); }
FVector2D UAurixSimWorld2D::AgentPosition(int32 A) const { return Handle ? FVector2D(D(sim_agent_get(Handle, (uint32)A, 0)), D(sim_agent_get(Handle, (uint32)A, 1))) : FVector2D::ZeroVector; }
FVector2D UAurixSimWorld2D::AgentVelocity(int32 A) const { return Handle ? FVector2D(D(sim_agent_get(Handle, (uint32)A, 2)), D(sim_agent_get(Handle, (uint32)A, 3))) : FVector2D::ZeroVector; }
bool UAurixSimWorld2D::AgentArrived(int32 A) const { return Handle && sim_agent_get(Handle, (uint32)A, 4) == 1; }
void UAurixSimWorld2D::CrowdStep() { if (Handle) sim_crowd_step(Handle); }
int64 UAurixSimWorld2D::CrowdHash() const { return Handle ? (int64)sim_crowd_hash(Handle) : 0; }
TArray<uint8> UAurixSimWorld2D::CrowdSnapshot() const { return Handle ? ReadBuffer(sim_crowd_snapshot(Handle)) : TArray<uint8>(); }
bool UAurixSimWorld2D::CrowdRestore(const TArray<uint8>& S) { if (!Handle) return false; LoadBuffer(S); return sim_crowd_restore(Handle) == 1; }

// ---------------------------------------------------------------- 3D
UAurixSimWorld3D* UAurixSimWorld3D::CreateWorld3D(UObject* Outer, FVector G, int32 TickHz)
{
	UAurixSimWorld3D* W = NewObject<UAurixSimWorld3D>(Outer ? Outer : (UObject*)GetTransientPackage());
	W->Handle = sim3_world_new(F(G.X), F(G.Y), F(G.Z), (uint32)FMath::Clamp(TickHz, 1, 1000));
	return W;
}
void UAurixSimWorld3D::BeginDestroy() { if (Handle) { sim3_world_free(Handle); Handle = 0; } Super::BeginDestroy(); }
void UAurixSimWorld3D::Step() { if (Handle) sim3_step(Handle); }
int64 UAurixSimWorld3D::Tick() const { return Handle ? (int64)sim3_tick(Handle) : -1; }
int64 UAurixSimWorld3D::Hash() const { return Handle ? (int64)sim3_hash(Handle) : 0; }
TArray<uint8> UAurixSimWorld3D::Snapshot() const
{
	TArray<uint8> Out; if (!Handle) return Out;
	const uint32 Len = sim3_snapshot(Handle); Out.SetNumUninitialized((int32)Len);
	for (uint32 I = 0; I < Len; ++I) Out[I] = (uint8)sim3_snapshot_byte(Handle, I);
	return Out;
}
bool UAurixSimWorld3D::Restore(const TArray<uint8>& S)
{
	if (!Handle) return false;
	sim3_snapshot_set_len(Handle, (uint32)S.Num());
	for (int32 I = 0; I < S.Num(); ++I) sim3_snapshot_set_byte(Handle, (uint32)I, S[I]);
	return sim3_restore(Handle) == 1;
}
int32 UAurixSimWorld3D::AddSphere(EAurixSimBodyKind K, FVector P, double R, double Den) { return Handle ? (int32)sim3_add_sphere(Handle, (uint32)K, F(P.X), F(P.Y), F(P.Z), F(R), F(Den)) : -1; }
int32 UAurixSimWorld3D::AddBox(EAurixSimBodyKind K, FVector P, FVector H, double Den) { return Handle ? (int32)sim3_add_box(Handle, (uint32)K, F(P.X), F(P.Y), F(P.Z), F(H.X), F(H.Y), F(H.Z), F(Den)) : -1; }
int32 UAurixSimWorld3D::AddCapsule(EAurixSimBodyKind K, FVector P, double HL, double R, double Den) { return Handle ? (int32)sim3_add_capsule(Handle, (uint32)K, F(P.X), F(P.Y), F(P.Z), F(HL), F(R), F(Den)) : -1; }
int32 UAurixSimWorld3D::AddConvex(EAurixSimBodyKind K, FVector P, const TArray<FVector>& V, const TArray<int32>& Idx, double Den)
{
	if (!Handle || V.Num() < 4 || Idx.Num() < 12 || Idx.Num() % 3) return -1;
	sim3_convex_reset(Handle);
	for (const FVector& X : V) sim3_convex_vert(Handle, F(X.X), F(X.Y), F(X.Z));
	for (int32 I = 0; I < Idx.Num(); I += 3) { sim3_convex_index(Handle, (uint32)Idx[I]); sim3_convex_index(Handle, (uint32)Idx[I + 1]); sim3_convex_index(Handle, (uint32)Idx[I + 2]); sim3_convex_face_end(Handle); }
	const uint32 R = sim3_add_convex(Handle, (uint32)K, F(P.X), F(P.Y), F(P.Z), F(Den));
	return R == 0xFFFFFFFFu ? -1 : (int32)R;
}
void UAurixSimWorld3D::RemoveBody(int32 B) { if (Handle) sim3_remove_body(Handle, (uint32)B); }
void UAurixSimWorld3D::SetMaterial(int32 B, double Fr, double Re, bool bS) { if (Handle) sim3_set_material(Handle, (uint32)B, F(Fr), F(Re), bS ? 1 : 0); }
void UAurixSimWorld3D::SetLayers(int32 B, int32 L, int32 M) { if (Handle) sim3_set_layers(Handle, (uint32)B, (uint32)L, (uint32)M); }
void UAurixSimWorld3D::SetVelocity(int32 B, FVector V, FVector W) { if (Handle) sim3_set_velocity(Handle, (uint32)B, F(V.X), F(V.Y), F(V.Z), F(W.X), F(W.Y), F(W.Z)); }
void UAurixSimWorld3D::SetPosition(int32 B, FVector P) { if (Handle) sim3_set_position(Handle, (uint32)B, F(P.X), F(P.Y), F(P.Z)); }
void UAurixSimWorld3D::SetRotation(int32 B, FQuat Q) { if (Handle) sim3_set_rotation(Handle, (uint32)B, F(Q.X), F(Q.Y), F(Q.Z), F(Q.W)); }
void UAurixSimWorld3D::ApplyImpulse(int32 B, FVector I) { if (Handle) sim3_apply_impulse(Handle, (uint32)B, F(I.X), F(I.Y), F(I.Z), 0, 0, 0, 0); }
void UAurixSimWorld3D::ApplyImpulseAt(int32 B, FVector I, FVector P) { if (Handle) sim3_apply_impulse(Handle, (uint32)B, F(I.X), F(I.Y), F(I.Z), 1, F(P.X), F(P.Y), F(P.Z)); }
int64 UAurixSimWorld3D::GetBodyRaw(int32 B, int32 Field) const { return Handle ? sim3_body_get(Handle, (uint32)B, (uint32)Field) : 0; }
FVector UAurixSimWorld3D::GetPosition(int32 B) const { return FVector(D(GetBodyRaw(B, 0)), D(GetBodyRaw(B, 1)), D(GetBodyRaw(B, 2))); }
FQuat UAurixSimWorld3D::GetRotation(int32 B) const { return FQuat(D(GetBodyRaw(B, 3)), D(GetBodyRaw(B, 4)), D(GetBodyRaw(B, 5)), D(GetBodyRaw(B, 6))); }
FVector UAurixSimWorld3D::GetVelocity(int32 B) const { return FVector(D(GetBodyRaw(B, 7)), D(GetBodyRaw(B, 8)), D(GetBodyRaw(B, 9))); }
FVector UAurixSimWorld3D::GetAngularVelocity(int32 B) const { return FVector(D(GetBodyRaw(B, 10)), D(GetBodyRaw(B, 11)), D(GetBodyRaw(B, 12))); }
bool UAurixSimWorld3D::ReadHit(uint32 R, FAurixSimHit& Hit) const
{
	if (R == 0) return false;
	Hit.Body = (int32)R - 1; Hit.Fraction = D(sim3_ray_get(0));
	Hit.Point = FVector(D(sim3_ray_get(1)), D(sim3_ray_get(2)), D(sim3_ray_get(3))); Hit.Normal = FVector(D(sim3_ray_get(4)), D(sim3_ray_get(5)), D(sim3_ray_get(6)));
	return true;
}
bool UAurixSimWorld3D::Raycast(FVector O, FVector Dr, int32 Mask, FAurixSimHit& Hit) const { return Handle && ReadHit(sim3_raycast(Handle, F(O.X), F(O.Y), F(O.Z), F(Dr.X), F(Dr.Y), F(Dr.Z), (uint32)Mask), Hit); }
bool UAurixSimWorld3D::SphereCast(FVector O, FVector Dr, double R, int32 Mask, FAurixSimHit& Hit) const { return Handle && ReadHit(sim3_spherecast(Handle, F(O.X), F(O.Y), F(O.Z), F(Dr.X), F(Dr.Y), F(Dr.Z), F(R), (uint32)Mask), Hit); }
bool UAurixSimWorld3D::CapsuleCast(FVector O, FQuat Q, double HL, double R, FVector Dr, int32 Mask, FAurixSimHit& Hit) const
{
	return Handle && ReadHit(sim3_capsulecast(Handle, F(O.X), F(O.Y), F(O.Z), F(Q.X), F(Q.Y), F(Q.Z), F(Q.W), F(HL), F(R), F(Dr.X), F(Dr.Y), F(Dr.Z), (uint32)Mask), Hit);
}
bool UAurixSimWorld3D::BoxCast(FVector O, FQuat Q, FVector H, FVector Dr, int32 Mask, FAurixSimHit& Hit) const
{
	return Handle && ReadHit(sim3_boxcast(Handle, F(O.X), F(O.Y), F(O.Z), F(Q.X), F(Q.Y), F(Q.Z), F(Q.W), F(H.X), F(H.Y), F(H.Z), F(Dr.X), F(Dr.Y), F(Dr.Z), (uint32)Mask), Hit);
}
TArray<FAurixSimContact> UAurixSimWorld3D::Contacts() const
{
	TArray<FAurixSimContact> Out;
	if (!Handle) return Out;
	const uint32 N = sim3_events_len(Handle);
	for (uint32 I = 0; I < N; ++I) { FAurixSimContact& C = Out.Emplace_GetRef(); C.BodyA = (int32)sim3_event_get(Handle, I, 0); C.BodyB = (int32)sim3_event_get(Handle, I, 1); C.bBegin = sim3_event_get(Handle, I, 2) == 1; }
	return Out;
}
int32 UAurixSimWorld3D::CreateCharacter(int32 Body, double StepH, double MinNy, double Snap) { return Handle ? (int32)sim3_kcc_new(Handle, (uint32)Body, F(StepH), F(MinNy), F(Snap)) : -1; }
bool UAurixSimWorld3D::MoveCharacter(int32 K, FVector Dl) { return Handle && sim3_kcc_move(Handle, (uint32)K, F(Dl.X), F(Dl.Y), F(Dl.Z)) == 1; }
FVector UAurixSimWorld3D::CharacterGroundNormal(int32 K) const { return Handle ? FVector(D(sim3_kcc_get(Handle, (uint32)K, 0)), D(sim3_kcc_get(Handle, (uint32)K, 1)), D(sim3_kcc_get(Handle, (uint32)K, 2))) : FVector::UpVector; }
FVector UAurixSimWorld3D::CharacterVelocity(int32 K) const { return Handle ? FVector(D(sim3_kcc_get(Handle, (uint32)K, 3)), D(sim3_kcc_get(Handle, (uint32)K, 4)), D(sim3_kcc_get(Handle, (uint32)K, 5))) : FVector::ZeroVector; }
bool UAurixSimWorld3D::CharacterGrounded(int32 K) const { return Handle && sim3_kcc_get(Handle, (uint32)K, 6) == 1; }
int32 UAurixSimWorld3D::JointBall(int32 A, int32 B, FVector AA, FVector AB) { return Handle ? (int32)sim3_joint_ball(Handle, (uint32)A, (uint32)B, F(AA.X), F(AA.Y), F(AA.Z), F(AB.X), F(AB.Y), F(AB.Z)) : -1; }
int32 UAurixSimWorld3D::JointHinge(int32 A, int32 B, FVector AA, FVector AB, FVector UA, FVector UB, bool bL, double Lo, double Up, double Sp, double Tq) { return Handle ? (int32)sim3_joint_hinge(Handle, (uint32)A, (uint32)B, F(AA.X), F(AA.Y), F(AA.Z), F(AB.X), F(AB.Y), F(AB.Z), F(UA.X), F(UA.Y), F(UA.Z), F(UB.X), F(UB.Y), F(UB.Z), bL ? 1 : 0, F(Lo), F(Up), F(Sp), F(Tq)) : -1; }
int32 UAurixSimWorld3D::JointDistance(int32 A, int32 B, FVector AA, FVector AB, double Len, double MinLen) { return Handle ? (int32)sim3_joint_distance(Handle, (uint32)A, (uint32)B, F(AA.X), F(AA.Y), F(AA.Z), F(AB.X), F(AB.Y), F(AB.Z), F(Len), F(MinLen)) : -1; }
void UAurixSimWorld3D::JointMotor(int32 J, double Sp, double Tq) { if (Handle) sim3_joint_motor(Handle, (uint32)J, F(Sp), F(Tq)); }
double UAurixSimWorld3D::HingeAngle(int32 J) const { return Handle ? D(sim3_hinge_angle(Handle, (uint32)J)) : 0; }
void UAurixSimWorld3D::RemoveJoint(int32 J) { if (Handle) sim3_joint_remove(Handle, (uint32)J); }
