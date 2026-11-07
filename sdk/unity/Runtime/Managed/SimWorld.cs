// Aurix Sim: deterministic fixed-point 2D physics over the shared net_sim_wasm native library
// (libnet_sim_wasm.so / .dll / .dylib; in WebGL the same module is linked as a static lib).
// Values crossing the boundary are Q32.32 raw longs; `Fx.From`/`Fx.To` convert.
using System;
using System.Runtime.InteropServices;

namespace Aurix.Net.Sim
{
    public static class Fx
    {
        public const long One = 1L << 32;
        public static long From(double v) => (long)Math.Round(v * 4294967296.0);
        public static double To(long raw) => raw / 4294967296.0;
    }

    public enum BodyKind : uint { Static = 0, Kinematic = 1, Dynamic = 2 }

    public struct BodyState { public double X, Y, Angle, Vx, Vy, AngularVelocity; }
    public struct RayHit { public uint Body; public double Fraction, X, Y, Nx, Ny; }
    public struct ContactEvent { public uint A, B; public bool Begin; }
    public struct AgentState { public double X, Y, Vx, Vy; public bool Arrived; public uint Poly; }

    internal static class SimNative
    {
        const string Lib = "net_sim_wasm";
        [DllImport(Lib)] public static extern uint sim_world_new(long gx, long gy, uint hz);
        [DllImport(Lib)] public static extern void sim_world_free(uint h);
        [DllImport(Lib)] public static extern void sim_step(uint h);
        [DllImport(Lib)] public static extern uint sim_tick(uint h);
        [DllImport(Lib)] public static extern ulong sim_hash(uint h);
        [DllImport(Lib)] public static extern uint sim_snapshot(uint h);
        [DllImport(Lib)] public static extern uint sim_snapshot_byte(uint h, uint i);
        [DllImport(Lib)] public static extern void sim_snapshot_set_len(uint h, uint len);
        [DllImport(Lib)] public static extern void sim_snapshot_set_byte(uint h, uint i, uint v);
        [DllImport(Lib)] public static extern uint sim_restore(uint h);
        [DllImport(Lib)] public static extern uint sim_add_circle(uint h, uint k, long x, long y, long r, long d);
        [DllImport(Lib)] public static extern uint sim_add_box(uint h, uint k, long x, long y, long hw, long hh, long d);
        [DllImport(Lib)] public static extern uint sim_add_capsule(uint h, uint k, long x, long y, long hl, long r, long d);
        [DllImport(Lib)] public static extern void sim_remove_body(uint h, uint id);
        [DllImport(Lib)] public static extern void sim_set_material(uint h, uint id, long f, long r);
        [DllImport(Lib)] public static extern void sim_set_layers(uint h, uint id, uint layer, uint mask, uint sensor);
        [DllImport(Lib)] public static extern void sim_set_velocity(uint h, uint id, long vx, long vy, long w);
        [DllImport(Lib)] public static extern void sim_set_position(uint h, uint id, long x, long y, long a);
        [DllImport(Lib)] public static extern void sim_apply_impulse(uint h, uint id, long ix, long iy);
        [DllImport(Lib)] public static extern long sim_body_get(uint h, uint id, uint field);
        [DllImport(Lib)] public static extern uint sim_raycast(uint h, long x, long y, long dx, long dy, uint mask);
        [DllImport(Lib)] public static extern long sim_ray_get(uint field);
        [DllImport(Lib)] public static extern uint sim_kcc_new(uint h, uint body, long step, long minNy);
        [DllImport(Lib)] public static extern uint sim_kcc_move(uint h, uint k, long dx, long dy);
        [DllImport(Lib)] public static extern ulong sim_golden_hash();
        [DllImport(Lib)] public static extern uint sim_nav_grid(uint h, long ox, long oy, long cell, uint cols, uint rows);
        [DllImport(Lib)] public static extern uint sim_nav_load(uint h);
        [DllImport(Lib)] public static extern ulong sim_nav_hash(uint h);
        [DllImport(Lib)] public static extern uint sim_nav_locate(uint h, long x, long y);
        [DllImport(Lib)] public static extern uint sim_nav_path(uint h, long x0, long y0, long x1, long y1, uint mask);
        [DllImport(Lib)] public static extern long sim_nav_point(uint h, uint i);
        [DllImport(Lib)] public static extern uint sim_nav_raycast(uint h, long x0, long y0, long x1, long y1);
        [DllImport(Lib)] public static extern uint sim_nav_field(uint h, long x, long y, uint mask);
        [DllImport(Lib)] public static extern uint sim_agent_add(uint h, long x, long y, long r, long speed);
        [DllImport(Lib)] public static extern void sim_agent_remove(uint h, uint id);
        [DllImport(Lib)] public static extern void sim_agent_target(uint h, uint id, long x, long y);
        [DllImport(Lib)] public static extern void sim_agent_follow(uint h, uint id, uint field);
        [DllImport(Lib)] public static extern void sim_agent_stop(uint h, uint id);
        [DllImport(Lib)] public static extern long sim_agent_get(uint h, uint id, uint field);
        [DllImport(Lib)] public static extern void sim_crowd_step(uint h);
        [DllImport(Lib)] public static extern ulong sim_crowd_hash(uint h);
        [DllImport(Lib)] public static extern uint sim_crowd_snapshot(uint h);
        [DllImport(Lib)] public static extern uint sim_crowd_restore(uint h);
        [DllImport(Lib)] public static extern ulong sim_nav_golden_hash();
        [DllImport(Lib)] public static extern uint sim_joint_distance(uint h, uint a, uint b, long ax, long ay, long bx, long by, long len, long minLen);
        [DllImport(Lib)] public static extern uint sim_joint_revolute(uint h, uint a, uint b, long ax, long ay, long bx, long by, uint hasLimits, long lower, long upper, long motorSpeed, long maxTorque);
        [DllImport(Lib)] public static extern uint sim_joint_weld(uint h, uint a, uint b, long ax, long ay, long bx, long by, long relAngle);
        [DllImport(Lib)] public static extern uint sim_joint_remove(uint h, uint id);
        [DllImport(Lib)] public static extern void sim_joint_motor(uint h, uint id, long speed, long maxTorque);
        [DllImport(Lib)] public static extern uint sim_events_len(uint h);
        [DllImport(Lib)] public static extern uint sim_event_get(uint h, uint i, uint field);
        [DllImport(Lib)] public static extern uint sim_circlecast(uint h, long x, long y, long dx, long dy, long r, uint mask);
        [DllImport(Lib)] public static extern ulong sim_joint_golden_hash();
    }

    public sealed class SimWorld : IDisposable
    {
        public readonly uint Handle;
        bool _freed;

        public SimWorld(double gravityX = 0, double gravityY = -10, uint tickHz = 60)
        {
            Handle = SimNative.sim_world_new(Fx.From(gravityX), Fx.From(gravityY), tickHz);
        }

        /// Reference scenario hash; equals the Rust/wasm value 0x58d9870ba45952b5 on every platform.
        public static ulong GoldenHash() => SimNative.sim_golden_hash();
        /// Navigation reference scenario hash; equals 0x746c5a608abe2f26 on every platform.
        public static ulong NavGoldenHash() => SimNative.sim_nav_golden_hash();
        /// Joint reference scenario hash; equals 0x4bcc781a484ceeed on every platform.
        public static ulong JointGoldenHash() => SimNative.sim_joint_golden_hash();

        public void Dispose() { if (!_freed) { _freed = true; SimNative.sim_world_free(Handle); } }
        public void Step() => SimNative.sim_step(Handle);
        public uint Tick => SimNative.sim_tick(Handle);
        public ulong Hash() => SimNative.sim_hash(Handle);

        public byte[] Snapshot()
        {
            uint n = SimNative.sim_snapshot(Handle);
            var b = new byte[n];
            for (uint i = 0; i < n; i++) b[i] = (byte)SimNative.sim_snapshot_byte(Handle, i);
            return b;
        }
        public bool Restore(byte[] bytes)
        {
            SimNative.sim_snapshot_set_len(Handle, (uint)bytes.Length);
            for (uint i = 0; i < bytes.Length; i++) SimNative.sim_snapshot_set_byte(Handle, i, bytes[i]);
            return SimNative.sim_restore(Handle) == 1;
        }

        public uint AddCircle(BodyKind kind, double x, double y, double radius, double density = 1)
            => SimNative.sim_add_circle(Handle, (uint)kind, Fx.From(x), Fx.From(y), Fx.From(radius), Fx.From(density));
        public uint AddBox(BodyKind kind, double x, double y, double halfW, double halfH, double density = 1)
            => SimNative.sim_add_box(Handle, (uint)kind, Fx.From(x), Fx.From(y), Fx.From(halfW), Fx.From(halfH), Fx.From(density));
        public uint AddCapsule(BodyKind kind, double x, double y, double halfLen, double radius, double density = 1)
            => SimNative.sim_add_capsule(Handle, (uint)kind, Fx.From(x), Fx.From(y), Fx.From(halfLen), Fx.From(radius), Fx.From(density));
        public void Remove(uint id) => SimNative.sim_remove_body(Handle, id);
        public void SetMaterial(uint id, double friction, double restitution) => SimNative.sim_set_material(Handle, id, Fx.From(friction), Fx.From(restitution));
        public void SetLayers(uint id, uint layer, uint mask, bool sensor = false) => SimNative.sim_set_layers(Handle, id, layer, mask, sensor ? 1u : 0u);
        public void SetVelocity(uint id, double vx, double vy, double angular = 0) => SimNative.sim_set_velocity(Handle, id, Fx.From(vx), Fx.From(vy), Fx.From(angular));
        public void SetPosition(uint id, double x, double y, double angle = 0) => SimNative.sim_set_position(Handle, id, Fx.From(x), Fx.From(y), Fx.From(angle));
        public void ApplyImpulse(uint id, double ix, double iy) => SimNative.sim_apply_impulse(Handle, id, Fx.From(ix), Fx.From(iy));

        public BodyState Body(uint id) => new BodyState
        {
            X = Fx.To(SimNative.sim_body_get(Handle, id, 0)),
            Y = Fx.To(SimNative.sim_body_get(Handle, id, 1)),
            Angle = Fx.To(SimNative.sim_body_get(Handle, id, 2)),
            Vx = Fx.To(SimNative.sim_body_get(Handle, id, 3)),
            Vy = Fx.To(SimNative.sim_body_get(Handle, id, 4)),
            AngularVelocity = Fx.To(SimNative.sim_body_get(Handle, id, 5)),
        };
        /// Raw Q32.32 field (0..5 = x, y, angle, vx, vy, angular velocity) for hashing/networking.
        public long BodyRaw(uint id, uint field) => SimNative.sim_body_get(Handle, id, field);

        public bool Raycast(double x, double y, double dx, double dy, out RayHit hit, uint mask = uint.MaxValue)
        {
            uint body = SimNative.sim_raycast(Handle, Fx.From(x), Fx.From(y), Fx.From(dx), Fx.From(dy), mask);
            hit = default;
            if (body == 0) return false;
            hit.Body = body;
            hit.Fraction = Fx.To(SimNative.sim_ray_get(0));
            hit.X = Fx.To(SimNative.sim_ray_get(1));
            hit.Y = Fx.To(SimNative.sim_ray_get(2));
            hit.Nx = Fx.To(SimNative.sim_ray_get(3));
            hit.Ny = Fx.To(SimNative.sim_ray_get(4));
            return true;
        }

        /// Character controller over a kinematic body; returns its index for MoveCharacter.
        public uint Character(uint body, double stepHeight = 0.5, double minGroundNormalY = 0.7)
            => SimNative.sim_kcc_new(Handle, body, Fx.From(stepHeight), Fx.From(minGroundNormalY));
        /// Moves the controller; true when grounded afterwards.
        public bool MoveCharacter(uint kcc, double dx, double dy) => SimNative.sim_kcc_move(Handle, kcc, Fx.From(dx), Fx.From(dy)) == 1;

        // ---- navigation: navmesh, paths, flow fields, crowd agents ----
        void FillBuffer(byte[] bytes)
        {
            SimNative.sim_snapshot_set_len(Handle, (uint)bytes.Length);
            for (uint i = 0; i < bytes.Length; i++) SimNative.sim_snapshot_set_byte(Handle, i, bytes[i]);
        }
        /// Grid navmesh of cols×rows cells; blocked = (col, row) cells to remove.
        public void NavGrid(double originX, double originY, double cell, uint cols, uint rows, (uint col, uint row)[] blocked = null)
        {
            blocked = blocked ?? Array.Empty<(uint, uint)>();
            var buf = new byte[blocked.Length * 8];
            for (int i = 0; i < blocked.Length; i++)
            {
                BitConverter.GetBytes(blocked[i].col).CopyTo(buf, i * 8);
                BitConverter.GetBytes(blocked[i].row).CopyTo(buf, i * 8 + 4);
            }
            FillBuffer(buf);
            SimNative.sim_nav_grid(Handle, Fx.From(originX), Fx.From(originY), Fx.From(cell), cols, rows);
        }
        /// Load a navmesh exported by NavMesh::to_bytes (JSON).
        public bool NavLoad(byte[] json) { FillBuffer(json); return SimNative.sim_nav_load(Handle) == 1; }
        public ulong NavHash() => SimNative.sim_nav_hash(Handle);
        public bool NavLocate(double x, double y, out uint poly) { poly = SimNative.sim_nav_locate(Handle, Fx.From(x), Fx.From(y)); return poly != uint.MaxValue; }
        public (double x, double y)[] NavPath(double x0, double y0, double x1, double y1, uint mask = uint.MaxValue)
        {
            uint n = SimNative.sim_nav_path(Handle, Fx.From(x0), Fx.From(y0), Fx.From(x1), Fx.From(y1), mask);
            if (n == 0) return null;
            var pts = new (double, double)[n];
            for (uint i = 0; i < n; i++) pts[i] = (Fx.To(SimNative.sim_nav_point(Handle, 2 * i)), Fx.To(SimNative.sim_nav_point(Handle, 2 * i + 1)));
            return pts;
        }
        /// Line of sight on the mesh; false when clear, else the boundary point hit.
        public bool NavRaycast(double x0, double y0, double x1, double y1, out double hx, out double hy)
        {
            bool hit = SimNative.sim_nav_raycast(Handle, Fx.From(x0), Fx.From(y0), Fx.From(x1), Fx.From(y1)) == 1;
            hx = hit ? Fx.To(SimNative.sim_ray_get(1)) : 0; hy = hit ? Fx.To(SimNative.sim_ray_get(2)) : 0;
            return hit;
        }
        /// Flow field towards a goal, shared by any number of agents; uint.MaxValue when unreachable.
        public uint NavField(double x, double y, uint mask = uint.MaxValue) => SimNative.sim_nav_field(Handle, Fx.From(x), Fx.From(y), mask);
        public uint AddAgent(double x, double y, double radius, double maxSpeed) => SimNative.sim_agent_add(Handle, Fx.From(x), Fx.From(y), Fx.From(radius), Fx.From(maxSpeed));
        public void RemoveAgent(uint id) => SimNative.sim_agent_remove(Handle, id);
        public void AgentTarget(uint id, double x, double y) => SimNative.sim_agent_target(Handle, id, Fx.From(x), Fx.From(y));
        public void AgentFollow(uint id, uint field) => SimNative.sim_agent_follow(Handle, id, field);
        public void AgentStop(uint id) => SimNative.sim_agent_stop(Handle, id);
        public AgentState Agent(uint id) => new AgentState
        {
            X = Fx.To(SimNative.sim_agent_get(Handle, id, 0)),
            Y = Fx.To(SimNative.sim_agent_get(Handle, id, 1)),
            Vx = Fx.To(SimNative.sim_agent_get(Handle, id, 2)),
            Vy = Fx.To(SimNative.sim_agent_get(Handle, id, 3)),
            Arrived = SimNative.sim_agent_get(Handle, id, 4) == 1,
            Poly = (uint)SimNative.sim_agent_get(Handle, id, 5),
        };
        /// Advance all agents one tick (call next to Step()).
        public void CrowdStep() => SimNative.sim_crowd_step(Handle);
        public ulong CrowdHash() => SimNative.sim_crowd_hash(Handle);
        public byte[] CrowdSnapshot()
        {
            uint n = SimNative.sim_crowd_snapshot(Handle);
            var b = new byte[n];
            for (uint i = 0; i < n; i++) b[i] = (byte)SimNative.sim_snapshot_byte(Handle, i);
            return b;
        }
        public bool CrowdRestore(byte[] bytes) { FillBuffer(bytes); return SimNative.sim_crowd_restore(Handle) == 1; }

        // ---- joints, contact events, shape casts ----
        /// Distance joint between local anchors; minLength < length allows slack (rope).
        public uint JointDistance(uint a, uint b, double ax, double ay, double bx, double by, double length, double? minLength = null)
            => SimNative.sim_joint_distance(Handle, a, b, Fx.From(ax), Fx.From(ay), Fx.From(bx), Fx.From(by), Fx.From(length), Fx.From(minLength ?? length));
        /// Pin joint; pass lower/upper for angle limits, maxTorque > 0 enables the motor.
        public uint JointRevolute(uint a, uint b, double ax, double ay, double bx, double by, double? lower = null, double? upper = null, double motorSpeed = 0, double maxTorque = 0)
        {
            bool limits = lower.HasValue && upper.HasValue;
            return SimNative.sim_joint_revolute(Handle, a, b, Fx.From(ax), Fx.From(ay), Fx.From(bx), Fx.From(by), limits ? 1u : 0u, Fx.From(lower ?? 0), Fx.From(upper ?? 0), Fx.From(motorSpeed), Fx.From(maxTorque));
        }
        public uint JointWeld(uint a, uint b, double ax, double ay, double bx, double by, double relAngle = 0)
            => SimNative.sim_joint_weld(Handle, a, b, Fx.From(ax), Fx.From(ay), Fx.From(bx), Fx.From(by), Fx.From(relAngle));
        public bool RemoveJoint(uint id) => SimNative.sim_joint_remove(Handle, id) == 1;
        public void SetMotor(uint joint, double speed, double maxTorque) => SimNative.sim_joint_motor(Handle, joint, Fx.From(speed), Fx.From(maxTorque));
        /// Contact begin/end events from the last Step().
        public ContactEvent[] Events()
        {
            uint n = SimNative.sim_events_len(Handle);
            var ev = new ContactEvent[n];
            for (uint i = 0; i < n; i++)
                ev[i] = new ContactEvent { A = SimNative.sim_event_get(Handle, i, 0), B = SimNative.sim_event_get(Handle, i, 1), Begin = SimNative.sim_event_get(Handle, i, 2) == 1 };
            return ev;
        }
        /// Sweep a circle along (dx, dy); same result as Raycast.
        public bool CircleCast(double x, double y, double dx, double dy, double radius, out RayHit hit, uint mask = uint.MaxValue)
        {
            uint r = SimNative.sim_circlecast(Handle, Fx.From(x), Fx.From(y), Fx.From(dx), Fx.From(dy), Fx.From(radius), mask);
            hit = default;
            if (r == 0) return false;
            hit.Body = r - 1;
            hit.Fraction = Fx.To(SimNative.sim_ray_get(0)); hit.X = Fx.To(SimNative.sim_ray_get(1)); hit.Y = Fx.To(SimNative.sim_ray_get(2));
            hit.Nx = Fx.To(SimNative.sim_ray_get(3)); hit.Ny = Fx.To(SimNative.sim_ray_get(4));
            return true;
        }
    }
}
