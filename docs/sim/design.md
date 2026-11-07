# aurix-sim — deterministic simulation kit (design)

Goal: a deterministic gameplay
engine that runs identically on every client (native and WASM) and, when wanted, on the node as a
referee. It stays a library the game links, not a service: lockstep transport, hashes, desync dumps,
rollback (`net_client::rollback`) and replays already exist; aurix-sim gives games something
deterministic to run on top of them without writing their own fixed-point physics.

Non-goals (for now): 3D rigid-body physics (phase 3), rendering/animation, a visual bot editor.

## Building blocks already in the repo

| Crate | Reused for |
|---|---|
| `net-fpmath` | `Fx` (Q32.32), `FxVec2/3`, `FxRng`, integer-only sqrt/trig |
| `net-sim` | `Frame`: entities, schema components, per-actor inputs, events, snapshot/restore/hash |
| `net-rollback` (server) / `net_client::rollback` | predict/rollback loops |
| `net-game` | WASM sandbox on the node (referee runs, server-side bots) |

## Phase 1 — `net-sim-physics` (2D)

Deterministic 2D rigid bodies in fixed point, API shaped after Box2D but without floats:

- **World**: gravity, fixed `dt = 1/tick_hz` (`Fx`), substeps, `step(&mut self)`; everything
  ordered by stable ids (`BodyId(u32)`, `ShapeId(u32)`) — no HashMap iteration, no pointers.
- **Bodies**: static / kinematic / dynamic; position, rotation (`Fx` angle, cached sin/cos in Fx),
  linear/angular velocity, mass/inertia from shapes, damping, bullet flag, sleep.
- **Shapes**: circle, capsule, convex polygon (≤ 8 vertices), edge chain for terrain; density,
  friction, restitution (combine: max friction, min restitution — overridable), collision
  layers/masks (`u32`), sensor flag.
- **Broadphase**: uniform grid (cell size = 2 × largest shape) with deterministic pair ordering
  (sorted by `(min_id, max_id)`), rebuilt incrementally; no SAP/float tree.
- **Narrowphase**: circle–circle, circle–polygon, polygon–polygon (SAT, clipped manifold ≤ 2 points),
  capsule via segment distance; manifold ids keyed on feature pairs for warm starting.
- **Solver**: sequential impulses, 8 velocity / 3 position iterations (configurable), Baumgarte +
  slop, restitution threshold, warm starting with accumulated impulses stored in the manifold;
  iteration order = sorted contact keys so every machine applies impulses in the same order.
- **Joints**: distance, revolute (with limits/motor), prismatic, weld, mouse/target — same solver.
- **Queries**: `raycast`, `shapecast`, `overlap_aabb/circle/polygon`, point test; results sorted by
  `(fraction, id)`.
- **Character controller (KCC)**: capsule mover with slope limit, step-up, ground snap, push-out;
  deterministic by construction (no engine callbacks).
- **Events**: begin/end contact, sensor enter/exit, collected per step in id order.
- **Snapshot/restore/hash**: whole world is `Clone` + `Serialize` (bytes via `postcard`), hash =
  FNV-1a over the canonical byte form — plugs straight into lockstep hash reports and dumps.
- **Determinism rules**: no `f32/f64` anywhere in the crate (`#![forbid]` via clippy lint
  `float_arithmetic` + a CI grep), no `HashMap` iteration, no `std::time`, no threads; every
  `Fx` op wraps/saturates explicitly.

Tests: unit tests per primitive; **golden-hash scenarios** (pyramid stack, bouncing balls,
ragdoll chain, KCC obstacle course) run N ticks and compare to committed hashes on x86_64, aarch64
and wasm32 (CI matrix) — the same vectors shipped to the C# and TS ports.

## Phase 2 — `net-sim-nav`

- Navmesh import (triangles from any baker: Recast output, Unity NavMesh export, hand-made) into a
  fixed-point mesh with regions and off-mesh links; serialization same as physics.
- A* over triangles + funnel (string pulling) → path of `FxVec2`; corridor refresh when the
  target moves.
- Steering agents: seek/arrive, separation (RVO-lite with deterministic neighbor order), avoidance
  of static obstacles; agent radius classes.
- Flow fields for crowds (per-target grid, integrated once per N ticks).
- Queries: nearest point on mesh, line-of-sight, random point in region.

## Phase 3 — 3D and bot kit

- 3D rigid bodies (sphere, capsule, box, convex hull, static triangle mesh), GJK/EPA in fixed
  point, same solver layout — scope decided after phase 1 numbers.
- Bot kit: deterministic HFSM + behaviour-tree runtime driven from `Frame` inputs, so bot seats
  from matchmaking (`bots`) can be filled by the same code on every client or on the node.

## Bindings

- **Rust**: the crates themselves (`net-sim-physics`, `net-sim-nav`).
- **WASM**: `cdylib` with a flat ABI (ids + Fx as i64) so Web and Unity (WebGL/IL2CPP) load the
  same `.wasm`; a thin TS/C# wrapper mirrors the API. Same bytes = same results everywhere.
- **Native C ABI** via `net-ffi` for C++/Godot.
- Node referee: the WASM build runs inside `net-game` to verify hashes or host bots.

## Milestones

1. `net-sim-physics` world + circles/polygons + grid broadphase + SI solver + raycast, golden
   hashes on native and wasm32.
2. Joints, capsules, edge chains, KCC, contact events; Web/C# wrappers over the WASM build;
   example lockstep game in `examples/`.
3. `net-sim-nav` A*/funnel/steering; flow fields.
4. 3D decision + bot kit.

## Status

- Phase 1 done: bodies (static/kinematic/dynamic), circles, boxes/convex polygons, capsules, edge
  chains; uniform-grid broadphase with sorted pairs; SAT manifolds over rounded convex cores;
  sequential impulses with warm starting, friction, restitution (pre-solve bias), positional
  correction; joints (distance/rope with slack, revolute with limits + motor, weld; jointed bodies
  do not collide); raycast (circle/polygon/capsule/segment); kinematic `CharacterController`
  (slide, step-up, ground snap, slope limit); contact/sensor events; snapshot/restore/FNV hash.
- Bindings: `net-sim-wasm` flat ABI (cdylib + wasm32), Web `Sim/SimWorld`, Unity
  `Aurix.Net.Sim.SimWorld`; golden hash `0x58d9870ba45952b5` identical on native and wasm32.
- Phase 2 done: `net-sim-nav` — convex-polygon navmesh with shared-edge adjacency and a grid
  builder, locate/nearest, A* over portals (BTreeSet frontier), funnel straightening, mesh
  raycast, per-area cost/flags, flow fields (Dijkstra per goal), `Crowd` (arrive, separation,
  sampled velocity-obstacle avoidance, border sliding, depenetration, snapshot/hash); exposed over
  the flat ABI + Web/Unity wrappers; nav golden hash `0x746c5a608abe2f26` native == wasm32.
- Phase 1 leftovers closed: `World::circle_cast` (sweep against shapes inflated by the radius),
  joints/motor retune, contact events and circle cast over the flat ABI + Web/Unity wrappers;
  joint golden hash `0x4bcc781a484ceeed` native == wasm32. Polygon/capsule sweeps are not
  implemented (circle cast covers characters and projectiles).
- Phase 3 (in progress): `net-sim-physics3d` — spheres, capsules, boxes (static/kinematic/
  dynamic), 3D grid broadphase, analytic sphere/capsule contacts, sphere/capsule vs box,
  box-box SAT (15 axes) with reference-face clipping (≤4 points), sequential impulses with warm
  starting, two-axis friction, restitution, positional correction, quaternion integration
  (`FxQuat`/`FxMat3` in net-fpmath), raycasts, snapshot/restore/hash; tests run two worlds and
  compare hashes; exposed as `sim3_*` over the flat ABI with Web `Sim3World` / Unity `Sim3World`
  wrappers, golden hash `0x0a9c7e6fc4fa43bf` native == wasm32. 3D joints (ball,
  hinge with limits/motor, distance/rope, `FxMat3` inverse for the point constraint) and a
  kinematic capsule `CharacterController` (push-out sliding, step-up with ledge probe, ground
  snap, slope filter) are in, exposed over `sim3_joint_*`/`sim3_kcc_*` with Web/Unity wrappers;
  golden hash `0x5db56af63cceee85` native == wasm32. Convex hulls (SAT faces + edge crosses, face clipping, hull pairs with every
  primitive, Cyrus–Beck raycast) and sphere sweeps (`sphere_cast`) are in, exposed as
  `sim3_convex_*`/`sim3_add_convex`/`sim3_spherecast` with Web/Unity wrappers; golden hash
  `0x29ab0385d38dc53f` native == wasm32. Capsule/box sweeps are not implemented.
- Bot kit: `net-sim-bots` — data-driven behavior trees (Sequence/Selector/Parallel, Condition/
  Action by id via the `Brain` trait, Inverter/Succeeder/Repeat/Cooldown/Timeout/Wait, blackboard
  Compare/Set), hierarchical state machines (parent transitions, on_enter/on_exit, initial
  children), integer `Blackboard`, tick-based timers, JSON authoring, snapshot/hash, and navmesh
  glue (`go_to`, `follow_field`, `stop`, `sync_agent`) over `net-sim-nav` crowds. Ported to
  TS (`sdk/web/src/bots.ts`) and C# (`Aurix.Net.Sim.Bots`), replaying the shared vector fixture.
