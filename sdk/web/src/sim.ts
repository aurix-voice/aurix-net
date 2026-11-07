// Aurix Sim (deterministic fixed-point 2D physics) over the shared `net_sim_wasm.wasm`.
// All values crossing the boundary are Q32.32 raw BigInts; `fx`/`num` convert.

export type Fx = bigint;
export const FX_ONE = 1n << 32n;
export function fx(n: number): Fx {
  return BigInt(Math.round(n * 2 ** 32));
}
export function num(v: Fx): number {
  return Number(v) / 2 ** 32;
}

export enum BodyKind {
  Static = 0,
  Kinematic = 1,
  Dynamic = 2,
}

interface SimExports {
  memory: WebAssembly.Memory;
  sim_world_new(gx: bigint, gy: bigint, hz: number): number;
  sim_world_free(h: number): void;
  sim_step(h: number): void;
  sim_tick(h: number): number;
  sim_hash(h: number): bigint;
  sim_snapshot(h: number): number;
  sim_snapshot_byte(h: number, i: number): number;
  sim_snapshot_set_len(h: number, len: number): void;
  sim_snapshot_set_byte(h: number, i: number, v: number): void;
  sim_restore(h: number): number;
  sim_add_circle(h: number, k: number, x: bigint, y: bigint, r: bigint, d: bigint): number;
  sim_add_box(h: number, k: number, x: bigint, y: bigint, hw: bigint, hh: bigint, d: bigint): number;
  sim_add_capsule(h: number, k: number, x: bigint, y: bigint, hl: bigint, r: bigint, d: bigint): number;
  sim_remove_body(h: number, id: number): void;
  sim_set_material(h: number, id: number, f: bigint, r: bigint): void;
  sim_set_layers(h: number, id: number, layer: number, mask: number, sensor: number): void;
  sim_set_velocity(h: number, id: number, vx: bigint, vy: bigint, w: bigint): void;
  sim_set_position(h: number, id: number, x: bigint, y: bigint, a: bigint): void;
  sim_apply_impulse(h: number, id: number, ix: bigint, iy: bigint): void;
  sim_body_get(h: number, id: number, field: number): bigint;
  sim_raycast(h: number, x: bigint, y: bigint, dx: bigint, dy: bigint, mask: number): number;
  sim_ray_get(field: number): bigint;
  sim_kcc_new(h: number, body: number, step: bigint, minNy: bigint): number;
  sim_kcc_move(h: number, k: number, dx: bigint, dy: bigint): number;
  sim_golden_hash(): bigint;
  sim_nav_grid(h: number, ox: bigint, oy: bigint, cell: bigint, cols: number, rows: number): number;
  sim_nav_load(h: number): number;
  sim_nav_hash(h: number): bigint;
  sim_nav_locate(h: number, x: bigint, y: bigint): number;
  sim_nav_path(h: number, x0: bigint, y0: bigint, x1: bigint, y1: bigint, mask: number): number;
  sim_nav_point(h: number, i: number): bigint;
  sim_nav_raycast(h: number, x0: bigint, y0: bigint, x1: bigint, y1: bigint): number;
  sim_nav_field(h: number, x: bigint, y: bigint, mask: number): number;
  sim_agent_add(h: number, x: bigint, y: bigint, r: bigint, speed: bigint): number;
  sim_agent_remove(h: number, id: number): void;
  sim_agent_target(h: number, id: number, x: bigint, y: bigint): void;
  sim_agent_follow(h: number, id: number, field: number): void;
  sim_agent_stop(h: number, id: number): void;
  sim_agent_get(h: number, id: number, field: number): bigint;
  sim_crowd_step(h: number): void;
  sim_crowd_hash(h: number): bigint;
  sim_crowd_snapshot(h: number): number;
  sim_crowd_restore(h: number): number;
  sim_nav_golden_hash(): bigint;
  sim_joint_distance(h: number, a: number, b: number, ax: bigint, ay: bigint, bx: bigint, by: bigint, len: bigint, minLen: bigint): number;
  sim_joint_revolute(h: number, a: number, b: number, ax: bigint, ay: bigint, bx: bigint, by: bigint, hasLimits: number, lower: bigint, upper: bigint, motorSpeed: bigint, maxTorque: bigint): number;
  sim_joint_weld(h: number, a: number, b: number, ax: bigint, ay: bigint, bx: bigint, by: bigint, relAngle: bigint): number;
  sim_joint_remove(h: number, id: number): number;
  sim_joint_motor(h: number, id: number, speed: bigint, maxTorque: bigint): void;
  sim_events_len(h: number): number;
  sim_event_get(h: number, i: number, field: number): number;
  sim_circlecast(h: number, x: bigint, y: bigint, dx: bigint, dy: bigint, r: bigint, mask: number): number;
  sim_joint_golden_hash(): bigint;
}

export interface BodyState {
  x: number;
  y: number;
  angle: number;
  vx: number;
  vy: number;
  angularVelocity: number;
}

export interface RayHit {
  body: number;
  fraction: number;
  point: { x: number; y: number };
  normal: { x: number; y: number };
}

/** Loads the sim module once; pass the wasm bytes or a compiled module. */
export class Sim {
  private constructor(private readonly e: SimExports) {}

  static async load(source: BufferSource | WebAssembly.Module | Response | Promise<Response>): Promise<Sim> {
    let module: WebAssembly.Module;
    if (source instanceof WebAssembly.Module) module = source;
    else if (source instanceof Response || source instanceof Promise) {
      module = (await WebAssembly.compileStreaming(source as Response)) as WebAssembly.Module;
    } else module = await WebAssembly.compile(source);
    const inst = await WebAssembly.instantiate(module, {});
    return new Sim(inst.exports as unknown as SimExports);
  }

  /** Reference scenario hash; must equal the native Rust value (`0x58d9870ba45952b5`). */
  goldenHash(): bigint {
    return this.e.sim_golden_hash();
  }
  /** Navigation reference scenario hash; must equal the native value (`0x746c5a608abe2f26`). */
  navGoldenHash(): bigint {
    return this.e.sim_nav_golden_hash();
  }
  /** Joint reference scenario hash; must equal the native value (`0x4bcc781a484ceeed`). */
  jointGoldenHash(): bigint {
    return this.e.sim_joint_golden_hash();
  }

  world(gravity = { x: 0, y: -10 }, tickHz = 60): SimWorld {
    return new SimWorld(this.e, this.e.sim_world_new(fx(gravity.x), fx(gravity.y), tickHz));
  }
}

export class SimWorld {
  constructor(private readonly e: SimExports, readonly handle: number) {}

  free() {
    this.e.sim_world_free(this.handle);
  }
  step() {
    this.e.sim_step(this.handle);
  }
  get tick(): number {
    return this.e.sim_tick(this.handle);
  }
  hash(): bigint {
    return this.e.sim_hash(this.handle);
  }
  snapshot(): Uint8Array {
    const n = this.e.sim_snapshot(this.handle);
    const out = new Uint8Array(n);
    for (let i = 0; i < n; i++) out[i] = this.e.sim_snapshot_byte(this.handle, i);
    return out;
  }
  restore(bytes: Uint8Array): boolean {
    this.e.sim_snapshot_set_len(this.handle, bytes.length);
    for (let i = 0; i < bytes.length; i++) this.e.sim_snapshot_set_byte(this.handle, i, bytes[i]);
    return this.e.sim_restore(this.handle) === 1;
  }

  addCircle(kind: BodyKind, x: number, y: number, radius: number, density = 1): number {
    return this.e.sim_add_circle(this.handle, kind, fx(x), fx(y), fx(radius), fx(density));
  }
  addBox(kind: BodyKind, x: number, y: number, halfW: number, halfH: number, density = 1): number {
    return this.e.sim_add_box(this.handle, kind, fx(x), fx(y), fx(halfW), fx(halfH), fx(density));
  }
  addCapsule(kind: BodyKind, x: number, y: number, halfLen: number, radius: number, density = 1): number {
    return this.e.sim_add_capsule(this.handle, kind, fx(x), fx(y), fx(halfLen), fx(radius), fx(density));
  }
  remove(id: number) {
    this.e.sim_remove_body(this.handle, id);
  }
  setMaterial(id: number, friction: number, restitution: number) {
    this.e.sim_set_material(this.handle, id, fx(friction), fx(restitution));
  }
  setLayers(id: number, layer: number, mask: number, sensor = false) {
    this.e.sim_set_layers(this.handle, id, layer >>> 0, mask >>> 0, sensor ? 1 : 0);
  }
  setVelocity(id: number, vx: number, vy: number, angular = 0) {
    this.e.sim_set_velocity(this.handle, id, fx(vx), fx(vy), fx(angular));
  }
  setPosition(id: number, x: number, y: number, angle = 0) {
    this.e.sim_set_position(this.handle, id, fx(x), fx(y), fx(angle));
  }
  applyImpulse(id: number, ix: number, iy: number) {
    this.e.sim_apply_impulse(this.handle, id, fx(ix), fx(iy));
  }
  body(id: number): BodyState {
    const g = (f: number) => num(this.e.sim_body_get(this.handle, id, f));
    return { x: g(0), y: g(1), angle: g(2), vx: g(3), vy: g(4), angularVelocity: g(5) };
  }
  /** Raw Q32.32 position, for hashing/networking without float round-trips. */
  bodyRaw(id: number, field: 0 | 1 | 2 | 3 | 4 | 5): Fx {
    return this.e.sim_body_get(this.handle, id, field);
  }
  raycast(x: number, y: number, dx: number, dy: number, mask = 0xffffffff): RayHit | null {
    const body = this.e.sim_raycast(this.handle, fx(x), fx(y), fx(dx), fx(dy), mask >>> 0);
    if (!body) return null;
    const r = (f: number) => num(this.e.sim_ray_get(f));
    return { body, fraction: r(0), point: { x: r(1), y: r(2) }, normal: { x: r(3), y: r(4) } };
  }
  /** Character controller over a kinematic body; returns its index for `moveCharacter`. */
  character(body: number, stepHeight = 0.5, minGroundNormalY = 0.7): number {
    return this.e.sim_kcc_new(this.handle, body, fx(stepHeight), fx(minGroundNormalY));
  }
  /** Moves the controller; returns true when grounded afterwards. */
  moveCharacter(kcc: number, dx: number, dy: number): boolean {
    return this.e.sim_kcc_move(this.handle, kcc, fx(dx), fx(dy)) === 1;
  }

  // ---- navigation (navmesh, paths, flow fields, crowd agents) ----
  private fillBuffer(bytes: Uint8Array) {
    this.e.sim_snapshot_set_len(this.handle, bytes.length);
    for (let i = 0; i < bytes.length; i++) this.e.sim_snapshot_set_byte(this.handle, i, bytes[i]);
  }
  /** Grid navmesh of cols×rows cells; `blocked` lists [col, row] cells to remove. */
  navGrid(origin: { x: number; y: number }, cell: number, cols: number, rows: number, blocked: [number, number][] = []) {
    const buf = new Uint8Array(blocked.length * 8);
    const dv = new DataView(buf.buffer);
    blocked.forEach(([c, r], i) => {
      dv.setUint32(i * 8, c, true);
      dv.setUint32(i * 8 + 4, r, true);
    });
    this.fillBuffer(buf);
    this.e.sim_nav_grid(this.handle, fx(origin.x), fx(origin.y), fx(cell), cols, rows);
  }
  /** Load a navmesh exported by `NavMesh::to_bytes` (JSON). */
  navLoad(bytes: Uint8Array): boolean {
    this.fillBuffer(bytes);
    return this.e.sim_nav_load(this.handle) === 1;
  }
  navHash(): bigint {
    return this.e.sim_nav_hash(this.handle);
  }
  navLocate(x: number, y: number): number | null {
    const p = this.e.sim_nav_locate(this.handle, fx(x), fx(y));
    return p === 0xffffffff ? null : p;
  }
  navPath(from: { x: number; y: number }, to: { x: number; y: number }, mask = 0xffffffff): { x: number; y: number }[] | null {
    const n = this.e.sim_nav_path(this.handle, fx(from.x), fx(from.y), fx(to.x), fx(to.y), mask >>> 0);
    if (!n) return null;
    const out = [];
    for (let i = 0; i < n; i++) out.push({ x: num(this.e.sim_nav_point(this.handle, 2 * i)), y: num(this.e.sim_nav_point(this.handle, 2 * i + 1)) });
    return out;
  }
  /** Line of sight on the mesh: null when clear, else the boundary point hit. */
  navRaycast(from: { x: number; y: number }, to: { x: number; y: number }): { x: number; y: number } | null {
    if (this.e.sim_nav_raycast(this.handle, fx(from.x), fx(from.y), fx(to.x), fx(to.y)) !== 1) return null;
    return { x: num(this.e.sim_ray_get(1)), y: num(this.e.sim_ray_get(2)) };
  }
  /** Flow field towards a goal shared by any number of agents; null when unreachable. */
  navField(goal: { x: number; y: number }, mask = 0xffffffff): number | null {
    const f = this.e.sim_nav_field(this.handle, fx(goal.x), fx(goal.y), mask >>> 0);
    return f === 0xffffffff ? null : f;
  }
  addAgent(x: number, y: number, radius: number, maxSpeed: number): number {
    return this.e.sim_agent_add(this.handle, fx(x), fx(y), fx(radius), fx(maxSpeed));
  }
  removeAgent(id: number) {
    this.e.sim_agent_remove(this.handle, id);
  }
  agentTarget(id: number, x: number, y: number) {
    this.e.sim_agent_target(this.handle, id, fx(x), fx(y));
  }
  agentFollow(id: number, field: number) {
    this.e.sim_agent_follow(this.handle, id, field);
  }
  agentStop(id: number) {
    this.e.sim_agent_stop(this.handle, id);
  }
  agent(id: number): { x: number; y: number; vx: number; vy: number; arrived: boolean; poly: number } {
    const g = (f: number) => this.e.sim_agent_get(this.handle, id, f);
    return { x: num(g(0)), y: num(g(1)), vx: num(g(2)), vy: num(g(3)), arrived: g(4) === 1n, poly: Number(g(5)) };
  }
  /** Advance all agents one tick (call next to `step()`). */
  crowdStep() {
    this.e.sim_crowd_step(this.handle);
  }
  crowdHash(): bigint {
    return this.e.sim_crowd_hash(this.handle);
  }
  crowdSnapshot(): Uint8Array {
    const n = this.e.sim_crowd_snapshot(this.handle);
    const out = new Uint8Array(n);
    for (let i = 0; i < n; i++) out[i] = this.e.sim_snapshot_byte(this.handle, i);
    return out;
  }
  crowdRestore(bytes: Uint8Array): boolean {
    this.fillBuffer(bytes);
    return this.e.sim_crowd_restore(this.handle) === 1;
  }

  // ---- joints, contact events, shape casts ----
  /** Distance joint between local anchors; `minLength < length` allows slack (rope). */
  jointDistance(a: number, b: number, anchorA: { x: number; y: number }, anchorB: { x: number; y: number }, length: number, minLength = length): number {
    return this.e.sim_joint_distance(this.handle, a, b, fx(anchorA.x), fx(anchorA.y), fx(anchorB.x), fx(anchorB.y), fx(length), fx(minLength));
  }
  /** Pin joint; optional angle limits and motor (`maxTorque > 0` enables it). */
  jointRevolute(
    a: number,
    b: number,
    anchorA: { x: number; y: number },
    anchorB: { x: number; y: number },
    opts: { lower?: number; upper?: number; motorSpeed?: number; maxTorque?: number } = {},
  ): number {
    const limits = opts.lower !== undefined && opts.upper !== undefined;
    return this.e.sim_joint_revolute(
      this.handle, a, b, fx(anchorA.x), fx(anchorA.y), fx(anchorB.x), fx(anchorB.y),
      limits ? 1 : 0, fx(opts.lower ?? 0), fx(opts.upper ?? 0), fx(opts.motorSpeed ?? 0), fx(opts.maxTorque ?? 0),
    );
  }
  jointWeld(a: number, b: number, anchorA: { x: number; y: number }, anchorB: { x: number; y: number }, relAngle = 0): number {
    return this.e.sim_joint_weld(this.handle, a, b, fx(anchorA.x), fx(anchorA.y), fx(anchorB.x), fx(anchorB.y), fx(relAngle));
  }
  removeJoint(id: number): boolean {
    return this.e.sim_joint_remove(this.handle, id) === 1;
  }
  setMotor(joint: number, speed: number, maxTorque: number) {
    this.e.sim_joint_motor(this.handle, joint, fx(speed), fx(maxTorque));
  }
  /** Contact begin/end events from the last `step()`. */
  events(): { a: number; b: number; begin: boolean }[] {
    const n = this.e.sim_events_len(this.handle);
    const out = [];
    for (let i = 0; i < n; i++) out.push({ a: this.e.sim_event_get(this.handle, i, 0), b: this.e.sim_event_get(this.handle, i, 1), begin: this.e.sim_event_get(this.handle, i, 2) === 1 });
    return out;
  }
  /** Sweep a circle along `dir`; same result shape as `raycast()`. */
  circleCast(from: { x: number; y: number }, dir: { x: number; y: number }, radius: number, mask = 0xffffffff) {
    const r = this.e.sim_circlecast(this.handle, fx(from.x), fx(from.y), fx(dir.x), fx(dir.y), fx(radius), mask >>> 0);
    if (!r) return null;
    const g = (i: number) => num(this.e.sim_ray_get(i));
    return { body: r - 1, fraction: g(0), point: { x: g(1), y: g(2) }, normal: { x: g(3), y: g(4) } };
  }
}
