using System;
using System.Runtime.InteropServices;

namespace Aurix.Net.Sim
{
    public struct Vec3 { public double X, Y, Z; public Vec3(double x, double y, double z) { X = x; Y = y; Z = z; } }
    public struct Quat
    {
        public double X, Y, Z, W;
        public Quat(double x, double y, double z, double w) { X = x; Y = y; Z = z; W = w; }
        /// Axis-angle helper (axis must be unit length).
        public static Quat AxisAngle(Vec3 axis, double angle)
        {
            double s = Math.Sin(angle / 2);
            return new Quat(axis.X * s, axis.Y * s, axis.Z * s, Math.Cos(angle / 2));
        }
    }
    public struct Body3State { public Vec3 Position, Velocity, AngularVelocity; public Quat Rotation; }
    public struct Character3State { public Vec3 GroundNormal, Velocity; public bool Grounded; public uint GroundBody; }
    public struct Ray3Hit { public uint Body; public double Fraction; public Vec3 Point, Normal; }

    internal static class Sim3Native
    {
        const string Lib = "net_sim_wasm";
        [DllImport(Lib)] public static extern uint sim3_world_new(long gx, long gy, long gz, uint hz);
        [DllImport(Lib)] public static extern void sim3_world_free(uint h);
        [DllImport(Lib)] public static extern void sim3_step(uint h);
        [DllImport(Lib)] public static extern ulong sim3_tick(uint h);
        [DllImport(Lib)] public static extern ulong sim3_hash(uint h);
        [DllImport(Lib)] public static extern uint sim3_snapshot(uint h);
        [DllImport(Lib)] public static extern uint sim3_snapshot_byte(uint h, uint i);
        [DllImport(Lib)] public static extern void sim3_snapshot_set_len(uint h, uint n);
        [DllImport(Lib)] public static extern void sim3_snapshot_set_byte(uint h, uint i, uint b);
        [DllImport(Lib)] public static extern uint sim3_restore(uint h);
        [DllImport(Lib)] public static extern uint sim3_add_sphere(uint h, uint k, long x, long y, long z, long r, long density);
        [DllImport(Lib)] public static extern uint sim3_add_box(uint h, uint k, long x, long y, long z, long hx, long hy, long hz, long density);
        [DllImport(Lib)] public static extern uint sim3_add_capsule(uint h, uint k, long x, long y, long z, long halfLen, long r, long density);
        [DllImport(Lib)] public static extern uint sim3_remove_body(uint h, uint id);
        [DllImport(Lib)] public static extern void sim3_set_material(uint h, uint id, long friction, long restitution, uint sensor);
        [DllImport(Lib)] public static extern void sim3_set_layers(uint h, uint id, uint layer, uint mask);
        [DllImport(Lib)] public static extern void sim3_set_velocity(uint h, uint id, long vx, long vy, long vz, long wx, long wy, long wz);
        [DllImport(Lib)] public static extern void sim3_set_position(uint h, uint id, long x, long y, long z);
        [DllImport(Lib)] public static extern void sim3_set_rotation(uint h, uint id, long qx, long qy, long qz, long qw);
        [DllImport(Lib)] public static extern void sim3_apply_impulse(uint h, uint id, long ix, long iy, long iz, uint atPoint, long px, long py, long pz);
        [DllImport(Lib)] public static extern long sim3_body_get(uint h, uint id, uint field);
        [DllImport(Lib)] public static extern uint sim3_raycast(uint h, long x, long y, long z, long dx, long dy, long dz, uint mask);
        [DllImport(Lib)] public static extern long sim3_ray_get(uint field);
        [DllImport(Lib)] public static extern uint sim3_events_len(uint h);
        [DllImport(Lib)] public static extern uint sim3_event_get(uint h, uint i, uint field);
        [DllImport(Lib)] public static extern ulong sim3_golden_hash();
        [DllImport(Lib)] public static extern uint sim3_joint_ball(uint h, uint a, uint b, long ax, long ay, long az, long bx, long by, long bz);
        [DllImport(Lib)] public static extern uint sim3_joint_hinge(uint h, uint a, uint b, long ax, long ay, long az, long bx, long by, long bz, long uax, long uay, long uaz, long ubx, long uby, long ubz, uint hasLimits, long lower, long upper, long motorSpeed, long maxTorque);
        [DllImport(Lib)] public static extern uint sim3_joint_distance(uint h, uint a, uint b, long ax, long ay, long az, long bx, long by, long bz, long len, long minLen);
        [DllImport(Lib)] public static extern uint sim3_joint_remove(uint h, uint id);
        [DllImport(Lib)] public static extern void sim3_joint_motor(uint h, uint id, long speed, long maxTorque);
        [DllImport(Lib)] public static extern long sim3_hinge_angle(uint h, uint id);
        [DllImport(Lib)] public static extern uint sim3_kcc_new(uint h, uint body, long stepHeight, long minGroundNy, long snap);
        [DllImport(Lib)] public static extern uint sim3_kcc_move(uint h, uint k, long dx, long dy, long dz);
        [DllImport(Lib)] public static extern long sim3_kcc_get(uint h, uint k, uint field);
        [DllImport(Lib)] public static extern ulong sim3_joint_golden_hash();
        [DllImport(Lib)] public static extern void sim3_convex_reset(uint h);
        [DllImport(Lib)] public static extern uint sim3_convex_vert(uint h, long x, long y, long z);
        [DllImport(Lib)] public static extern void sim3_convex_index(uint h, uint i);
        [DllImport(Lib)] public static extern uint sim3_convex_face_end(uint h);
        [DllImport(Lib)] public static extern uint sim3_add_convex(uint h, uint k, long x, long y, long z, long density);
        [DllImport(Lib)] public static extern uint sim3_spherecast(uint h, long x, long y, long z, long dx, long dy, long dz, long radius, uint mask);
        [DllImport(Lib)] public static extern ulong sim3_hull_golden_hash();
    }

    /// Deterministic fixed-point 3D world (spheres, boxes, capsules) over the native/wasm module.
    public sealed class Sim3World : IDisposable
    {
        public uint Handle { get; private set; }
        public Sim3World(double gx = 0, double gy = -10, double gz = 0, uint tickHz = 60)
        {
            Handle = Sim3Native.sim3_world_new(Fx.From(gx), Fx.From(gy), Fx.From(gz), tickHz);
        }
        public void Dispose() { if (Handle != uint.MaxValue) { Sim3Native.sim3_world_free(Handle); Handle = uint.MaxValue; } }
        /// 3D reference scenario hash; equals 0x0a9c7e6fc4fa43bf on every platform.
        public static ulong GoldenHash() => Sim3Native.sim3_golden_hash();
        /// 3D joints/character reference scenario hash; equals 0x5db56af63cceee85 on every platform.
        public static ulong JointGoldenHash() => Sim3Native.sim3_joint_golden_hash();
        /// 3D convex hull reference scenario hash; equals 0x29ab0385d38dc53f on every platform.
        public static ulong HullGoldenHash() => Sim3Native.sim3_hull_golden_hash();

        public void Step() => Sim3Native.sim3_step(Handle);
        public ulong Tick => Sim3Native.sim3_tick(Handle);
        public ulong Hash() => Sim3Native.sim3_hash(Handle);
        public byte[] Snapshot()
        {
            uint n = Sim3Native.sim3_snapshot(Handle);
            var b = new byte[n];
            for (uint i = 0; i < n; i++) b[i] = (byte)Sim3Native.sim3_snapshot_byte(Handle, i);
            return b;
        }
        public bool Restore(byte[] bytes)
        {
            Sim3Native.sim3_snapshot_set_len(Handle, (uint)bytes.Length);
            for (uint i = 0; i < bytes.Length; i++) Sim3Native.sim3_snapshot_set_byte(Handle, i, bytes[i]);
            return Sim3Native.sim3_restore(Handle) == 1;
        }
        public uint AddSphere(BodyKind kind, Vec3 p, double radius, double density = 1)
            => Sim3Native.sim3_add_sphere(Handle, (uint)kind, Fx.From(p.X), Fx.From(p.Y), Fx.From(p.Z), Fx.From(radius), Fx.From(density));
        public uint AddBox(BodyKind kind, Vec3 p, Vec3 half, double density = 1)
            => Sim3Native.sim3_add_box(Handle, (uint)kind, Fx.From(p.X), Fx.From(p.Y), Fx.From(p.Z), Fx.From(half.X), Fx.From(half.Y), Fx.From(half.Z), Fx.From(density));
        /// Capsule along the body y axis.
        public uint AddCapsule(BodyKind kind, Vec3 p, double halfLen, double radius, double density = 1)
            => Sim3Native.sim3_add_capsule(Handle, (uint)kind, Fx.From(p.X), Fx.From(p.Y), Fx.From(p.Z), Fx.From(halfLen), Fx.From(radius), Fx.From(density));
        public bool Remove(uint id) => Sim3Native.sim3_remove_body(Handle, id) == 1;
        public void SetMaterial(uint id, double friction, double restitution, bool sensor = false)
            => Sim3Native.sim3_set_material(Handle, id, Fx.From(friction), Fx.From(restitution), sensor ? 1u : 0u);
        public void SetLayers(uint id, uint layer, uint mask) => Sim3Native.sim3_set_layers(Handle, id, layer, mask);
        public void SetVelocity(uint id, Vec3 v, Vec3 w = default)
            => Sim3Native.sim3_set_velocity(Handle, id, Fx.From(v.X), Fx.From(v.Y), Fx.From(v.Z), Fx.From(w.X), Fx.From(w.Y), Fx.From(w.Z));
        public void SetPosition(uint id, Vec3 p) => Sim3Native.sim3_set_position(Handle, id, Fx.From(p.X), Fx.From(p.Y), Fx.From(p.Z));
        public void SetRotation(uint id, Quat q) => Sim3Native.sim3_set_rotation(Handle, id, Fx.From(q.X), Fx.From(q.Y), Fx.From(q.Z), Fx.From(q.W));
        public void ApplyImpulse(uint id, Vec3 impulse, Vec3? at = null)
            => Sim3Native.sim3_apply_impulse(Handle, id, Fx.From(impulse.X), Fx.From(impulse.Y), Fx.From(impulse.Z), at.HasValue ? 1u : 0u,
                Fx.From(at?.X ?? 0), Fx.From(at?.Y ?? 0), Fx.From(at?.Z ?? 0));
        public Body3State Body(uint id)
        {
            double G(uint f) => Fx.To(Sim3Native.sim3_body_get(Handle, id, f));
            return new Body3State
            {
                Position = new Vec3(G(0), G(1), G(2)),
                Rotation = new Quat(G(3), G(4), G(5), G(6)),
                Velocity = new Vec3(G(7), G(8), G(9)),
                AngularVelocity = new Vec3(G(10), G(11), G(12)),
            };
        }
        public bool Raycast(Vec3 from, Vec3 dir, out Ray3Hit hit, uint mask = uint.MaxValue)
        {
            uint r = Sim3Native.sim3_raycast(Handle, Fx.From(from.X), Fx.From(from.Y), Fx.From(from.Z), Fx.From(dir.X), Fx.From(dir.Y), Fx.From(dir.Z), mask);
            hit = default;
            if (r == 0) return false;
            double G(uint f) => Fx.To(Sim3Native.sim3_ray_get(f));
            hit.Body = r - 1; hit.Fraction = G(0);
            hit.Point = new Vec3(G(1), G(2), G(3)); hit.Normal = new Vec3(G(4), G(5), G(6));
            return true;
        }
        public ContactEvent[] Events()
        {
            uint n = Sim3Native.sim3_events_len(Handle);
            var ev = new ContactEvent[n];
            for (uint i = 0; i < n; i++)
                ev[i] = new ContactEvent { A = Sim3Native.sim3_event_get(Handle, i, 0), B = Sim3Native.sim3_event_get(Handle, i, 1), Begin = Sim3Native.sim3_event_get(Handle, i, 2) == 1 };
            return ev;
        }

        // ---- joints and character controller ----
        public uint JointBall(uint a, uint b, Vec3 anchorA, Vec3 anchorB)
            => Sim3Native.sim3_joint_ball(Handle, a, b, Fx.From(anchorA.X), Fx.From(anchorA.Y), Fx.From(anchorA.Z), Fx.From(anchorB.X), Fx.From(anchorB.Y), Fx.From(anchorB.Z));
        /// Hinge around local axes; pass lower/upper for angle limits (radians), maxTorque > 0 enables the motor.
        public uint JointHinge(uint a, uint b, Vec3 anchorA, Vec3 anchorB, Vec3 axisA, Vec3 axisB, double? lower = null, double? upper = null, double motorSpeed = 0, double maxTorque = 0)
        {
            bool limits = lower.HasValue && upper.HasValue;
            return Sim3Native.sim3_joint_hinge(Handle, a, b, Fx.From(anchorA.X), Fx.From(anchorA.Y), Fx.From(anchorA.Z), Fx.From(anchorB.X), Fx.From(anchorB.Y), Fx.From(anchorB.Z),
                Fx.From(axisA.X), Fx.From(axisA.Y), Fx.From(axisA.Z), Fx.From(axisB.X), Fx.From(axisB.Y), Fx.From(axisB.Z),
                limits ? 1u : 0u, Fx.From(lower ?? 0), Fx.From(upper ?? 0), Fx.From(motorSpeed), Fx.From(maxTorque));
        }
        /// Distance joint; minLength < length allows slack (rope).
        public uint JointDistance(uint a, uint b, Vec3 anchorA, Vec3 anchorB, double length, double? minLength = null)
            => Sim3Native.sim3_joint_distance(Handle, a, b, Fx.From(anchorA.X), Fx.From(anchorA.Y), Fx.From(anchorA.Z), Fx.From(anchorB.X), Fx.From(anchorB.Y), Fx.From(anchorB.Z), Fx.From(length), Fx.From(minLength ?? length));
        public bool RemoveJoint(uint id) => Sim3Native.sim3_joint_remove(Handle, id) == 1;
        public void SetMotor(uint joint, double speed, double maxTorque) => Sim3Native.sim3_joint_motor(Handle, joint, Fx.From(speed), Fx.From(maxTorque));
        public double HingeAngle(uint joint) => Fx.To(Sim3Native.sim3_hinge_angle(Handle, joint));
        /// Capsule character controller over a kinematic capsule body; returns a controller index.
        public uint Character(uint body, double stepHeight = 0.5, double minGroundNormalY = 0.7, double snapDistance = 0.25)
            => Sim3Native.sim3_kcc_new(Handle, body, Fx.From(stepHeight), Fx.From(minGroundNormalY), Fx.From(snapDistance));
        /// Move the controller this tick (call before Step()); returns whether it stands on ground.
        public bool MoveCharacter(uint k, Vec3 delta) => Sim3Native.sim3_kcc_move(Handle, k, Fx.From(delta.X), Fx.From(delta.Y), Fx.From(delta.Z)) == 1;
        public Character3State CharacterState(uint k)
        {
            double G(uint f) => Fx.To(Sim3Native.sim3_kcc_get(Handle, k, f));
            return new Character3State
            {
                GroundNormal = new Vec3(G(0), G(1), G(2)),
                Velocity = new Vec3(G(3), G(4), G(5)),
                Grounded = Sim3Native.sim3_kcc_get(Handle, k, 6) == 1,
                GroundBody = (uint)Sim3Native.sim3_kcc_get(Handle, k, 7),
            };
        }

        // ---- convex hulls and sweeps ----
        /// Convex polyhedron from body-space vertices (<= 32) and faces as index loops (any winding);
        /// author vertices around the centre of mass. Throws on an invalid hull.
        public uint AddConvex(BodyKind kind, Vec3 pos, Vec3[] verts, uint[][] faces, double density = 1)
        {
            Sim3Native.sim3_convex_reset(Handle);
            foreach (var v in verts) Sim3Native.sim3_convex_vert(Handle, Fx.From(v.X), Fx.From(v.Y), Fx.From(v.Z));
            foreach (var f in faces) { foreach (var i in f) Sim3Native.sim3_convex_index(Handle, i); Sim3Native.sim3_convex_face_end(Handle); }
            uint id = Sim3Native.sim3_add_convex(Handle, (uint)kind, Fx.From(pos.X), Fx.From(pos.Y), Fx.From(pos.Z), Fx.From(density));
            if (id == 0) throw new ArgumentException("sim3: invalid convex hull (need >= 4 vertices and >= 4 faces)");
            return id - 1;
        }
        /// Sweep a sphere along dir; Fraction is where the centre stops, Point/Normal lie on the hit surface.
        public bool SphereCast(Vec3 from, Vec3 dir, double radius, out Ray3Hit hit, uint mask = uint.MaxValue)
        {
            uint id = Sim3Native.sim3_spherecast(Handle, Fx.From(from.X), Fx.From(from.Y), Fx.From(from.Z), Fx.From(dir.X), Fx.From(dir.Y), Fx.From(dir.Z), Fx.From(radius), mask);
            hit = default;
            if (id == 0) return false;
            double G(uint i) => Fx.To(Sim3Native.sim3_ray_get(i));
            hit = new Ray3Hit { Body = id - 1, Fraction = G(0), Point = new Vec3(G(1), G(2), G(3)), Normal = new Vec3(G(4), G(5), G(6)) };
            return true;
        }
    }
}
