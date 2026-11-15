#pragma once
#include "CoreMinimal.h"
#include "UObject/Object.h"
#include "AurixSimWorld.generated.h"

/** Body kind for aurix-sim: 0 static, 1 kinematic, 2 dynamic. */
UENUM(BlueprintType)
enum class EAurixSimBodyKind : uint8 { Static = 0, Kinematic = 1, Dynamic = 2 };

USTRUCT(BlueprintType)
struct AURIXNET_API FAurixSimHit
{
	GENERATED_BODY()
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Sim") int32 Body = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Sim") double Fraction = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Sim") FVector Point = FVector::ZeroVector;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Sim") FVector Normal = FVector::ZeroVector;
};

USTRUCT(BlueprintType)
struct AURIXNET_API FAurixSimContact
{
	GENERATED_BODY()
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Sim") int32 BodyA = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Sim") int32 BodyB = 0;
	UPROPERTY(BlueprintReadOnly, Category = "Aurix Sim") bool bBegin = true;
};

/** Fixed-point helpers: aurix-sim works in Q32.32 raw int64; these convert from/to doubles (units are yours — metres recommended). */
UCLASS()
class AURIXNET_API UAurixFixedPoint : public UBlueprintFunctionLibrary
{
	GENERATED_BODY()
public:
	static int64 Fx(double V) { return (int64)(V * 4294967296.0); }
	static double Dx(int64 V) { return (double)V / 4294967296.0; }
	/** Q32.32 raw value of a double (deterministic only if every client computes it from the same input). */
	UFUNCTION(BlueprintPure, Category = "Aurix Sim") static int64 ToFixed(double Value) { return Fx(Value); }
	UFUNCTION(BlueprintPure, Category = "Aurix Sim") static double FromFixed(int64 Raw) { return Dx(Raw); }
};

/**
 * Deterministic 2D world (aurix-sim): fixed-point rigid bodies, joints, raycasts, a kinematic
 * character controller, grid navmesh with flow fields and a crowd. Same results on every machine
 * and on the cell (WASM), so hashes compare and rollback/replays are exact. Coordinates are doubles
 * in your units; they are converted to Q32.32 at the boundary.
 */
UCLASS(BlueprintType)
class AURIXNET_API UAurixSimWorld2D : public UObject
{
	GENERATED_BODY()
public:
	virtual void BeginDestroy() override;

	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D", meta = (WorldContext = "Outer")) static UAurixSimWorld2D* CreateWorld2D(UObject* Outer, FVector2D Gravity, int32 TickHz = 60);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") void Step();
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|2D") int64 Tick() const;
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|2D") int64 Hash() const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") TArray<uint8> Snapshot() const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") bool Restore(const TArray<uint8>& State);

	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") int32 AddCircle(EAurixSimBodyKind Kind, FVector2D Position, double Radius, double Density = 1.0);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") int32 AddBox(EAurixSimBodyKind Kind, FVector2D Position, FVector2D HalfExtents, double Density = 1.0);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") int32 AddCapsule(EAurixSimBodyKind Kind, FVector2D Position, double HalfLength, double Radius, double Density = 1.0);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") void RemoveBody(int32 Body);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") void SetMaterial(int32 Body, double Friction, double Restitution);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") void SetLayers(int32 Body, int32 Layer, int32 Mask, bool bSensor);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") void SetVelocity(int32 Body, FVector2D Velocity, double AngularVelocity);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") void SetPosition(int32 Body, FVector2D Position, double Angle);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") void ApplyImpulse(int32 Body, FVector2D Impulse);
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|2D") FVector2D GetPosition(int32 Body) const;
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|2D") double GetAngle(int32 Body) const;
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|2D") FVector2D GetVelocity(int32 Body) const;
	/** Raw Q32.32 body field (0..=4 x, y, angle, vx, vy, 5 angular velocity) for deterministic game logic. */
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|2D") int64 GetBodyRaw(int32 Body, int32 Field) const;

	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") bool Raycast(FVector2D Origin, FVector2D Direction, int32 Mask, FAurixSimHit& Hit) const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") bool CircleCast(FVector2D Origin, FVector2D Direction, double Radius, int32 Mask, FAurixSimHit& Hit) const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") TArray<FAurixSimContact> Contacts() const;

	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") int32 CreateCharacter(int32 KinematicBody, double StepHeight, double MinGroundNormalY);
	/** Move the character this tick; returns true when grounded afterwards. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") bool MoveCharacter(int32 Controller, FVector2D Delta);

	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") int32 JointDistance(int32 A, int32 B, FVector2D AnchorA, FVector2D AnchorB, double Length, double MinLength = 0.0);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") int32 JointRevolute(int32 A, int32 B, FVector2D AnchorA, FVector2D AnchorB, bool bLimits = false, double Lower = 0.0, double Upper = 0.0, double MotorSpeed = 0.0, double MaxTorque = 0.0);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") int32 JointWeld(int32 A, int32 B, FVector2D AnchorA, FVector2D AnchorB, double Angle);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") void JointMotor(int32 Joint, double Speed, double MaxTorque);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|2D") void RemoveJoint(int32 Joint);

	/** Grid navmesh: Cols x Rows cells of Cell size from Origin; Blocked = (col,row) pairs. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|Nav") bool NavGrid(FVector2D Origin, double Cell, int32 Cols, int32 Rows, const TArray<FIntPoint>& Blocked);
	/** Load a navmesh from its JSON bytes (aurix-sim NavMesh::to_bytes). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|Nav") bool NavLoad(const TArray<uint8>& NavJson);
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|Nav") int64 NavHash() const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|Nav") TArray<FVector2D> NavPath(FVector2D From, FVector2D To, int32 Mask);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|Nav") bool NavRaycast(FVector2D From, FVector2D To, FVector2D& HitPoint) const;
	/** Flow field towards a point; returns its index or -1 when unreachable. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|Nav") int32 NavFlowField(FVector2D To, int32 Mask);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|Nav") int32 AgentAdd(FVector2D Position, double Radius, double MaxSpeed);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|Nav") void AgentRemove(int32 Agent);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|Nav") void AgentTarget(int32 Agent, FVector2D To);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|Nav") void AgentFollow(int32 Agent, int32 FlowField);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|Nav") void AgentStop(int32 Agent);
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|Nav") FVector2D AgentPosition(int32 Agent) const;
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|Nav") FVector2D AgentVelocity(int32 Agent) const;
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|Nav") bool AgentArrived(int32 Agent) const;
	/** Advance the crowd one tick (call after Step). */
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|Nav") void CrowdStep();
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|Nav") int64 CrowdHash() const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|Nav") TArray<uint8> CrowdSnapshot() const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|Nav") bool CrowdRestore(const TArray<uint8>& State);

	bool IsValidWorld() const { return Handle != 0; }
private:
	uint32 Handle = 0;
	void LoadBuffer(const TArray<uint8>& B) const;
	TArray<uint8> ReadBuffer(uint32 Len) const;
	bool ReadHit(uint32 R, FAurixSimHit& Hit) const;
};

/** Deterministic 3D world (aurix-sim): rigid bodies (sphere, box, capsule, convex), joints, sweeps, capsule character controller. */
UCLASS(BlueprintType)
class AURIXNET_API UAurixSimWorld3D : public UObject
{
	GENERATED_BODY()
public:
	virtual void BeginDestroy() override;

	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D", meta = (WorldContext = "Outer")) static UAurixSimWorld3D* CreateWorld3D(UObject* Outer, FVector Gravity, int32 TickHz = 60);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") void Step();
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|3D") int64 Tick() const;
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|3D") int64 Hash() const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") TArray<uint8> Snapshot() const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") bool Restore(const TArray<uint8>& State);

	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") int32 AddSphere(EAurixSimBodyKind Kind, FVector Position, double Radius, double Density = 1.0);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") int32 AddBox(EAurixSimBodyKind Kind, FVector Position, FVector HalfExtents, double Density = 1.0);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") int32 AddCapsule(EAurixSimBodyKind Kind, FVector Position, double HalfLength, double Radius, double Density = 1.0);
	/** Convex hull: vertices + triangle indices (3 per face). Returns body id or -1 for an invalid hull. */
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") int32 AddConvex(EAurixSimBodyKind Kind, FVector Position, const TArray<FVector>& Vertices, const TArray<int32>& Indices, double Density = 1.0);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") void RemoveBody(int32 Body);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") void SetMaterial(int32 Body, double Friction, double Restitution, bool bSensor);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") void SetLayers(int32 Body, int32 Layer, int32 Mask);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") void SetVelocity(int32 Body, FVector Linear, FVector Angular);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") void SetPosition(int32 Body, FVector Position);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") void SetRotation(int32 Body, FQuat Rotation);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") void ApplyImpulse(int32 Body, FVector Impulse);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") void ApplyImpulseAt(int32 Body, FVector Impulse, FVector WorldPoint);
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|3D") FVector GetPosition(int32 Body) const;
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|3D") FQuat GetRotation(int32 Body) const;
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|3D") FVector GetVelocity(int32 Body) const;
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|3D") FVector GetAngularVelocity(int32 Body) const;
	/** Raw Q32.32 body field: 0..=2 position, 3..=6 rotation xyzw, 7..=9 velocity, 10..=12 angular velocity. */
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|3D") int64 GetBodyRaw(int32 Body, int32 Field) const;

	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") bool Raycast(FVector Origin, FVector Direction, int32 Mask, FAurixSimHit& Hit) const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") bool SphereCast(FVector Origin, FVector Direction, double Radius, int32 Mask, FAurixSimHit& Hit) const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") bool CapsuleCast(FVector Origin, FQuat Rotation, double HalfLength, double Radius, FVector Direction, int32 Mask, FAurixSimHit& Hit) const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") bool BoxCast(FVector Origin, FQuat Rotation, FVector HalfExtents, FVector Direction, int32 Mask, FAurixSimHit& Hit) const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") TArray<FAurixSimContact> Contacts() const;

	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") int32 CreateCharacter(int32 KinematicCapsule, double StepHeight, double MinGroundNormalY, double Snap);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") bool MoveCharacter(int32 Controller, FVector Delta);
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|3D") FVector CharacterGroundNormal(int32 Controller) const;
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|3D") FVector CharacterVelocity(int32 Controller) const;
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|3D") bool CharacterGrounded(int32 Controller) const;

	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") int32 JointBall(int32 A, int32 B, FVector AnchorA, FVector AnchorB);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") int32 JointHinge(int32 A, int32 B, FVector AnchorA, FVector AnchorB, FVector AxisA, FVector AxisB, bool bLimits = false, double Lower = 0.0, double Upper = 0.0, double MotorSpeed = 0.0, double MaxTorque = 0.0);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") int32 JointDistance(int32 A, int32 B, FVector AnchorA, FVector AnchorB, double Length, double MinLength = 0.0);
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") void JointMotor(int32 Joint, double Speed, double MaxTorque);
	UFUNCTION(BlueprintPure, Category = "Aurix Sim|3D") double HingeAngle(int32 Joint) const;
	UFUNCTION(BlueprintCallable, Category = "Aurix Sim|3D") void RemoveJoint(int32 Joint);

	bool IsValidWorld() const { return Handle != 0; }
private:
	uint32 Handle = 0;
	bool ReadHit(uint32 R, FAurixSimHit& Hit) const;
};
