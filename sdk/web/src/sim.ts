// Aurix Sim (deterministic fixed-point 2D physics) over the shared `net_sim_wasm.wasm`.
// All values crossing the boundary are Q32.32 raw BigInts; `fx`/`num` convert.

export type Fx = bigint;
export const FX_ONE = 1n << 32n;
export function fx(length: number): Fx {
  return BigInt(Math.round(length * 2 ** 32));
}
export function num(value: Fx): number {
  return Number(value) / 2 ** 32;
}

export enum BodyKind {
  Static = 0,
  Kinematic = 1,
  Dynamic = 2,
}

interface SimExports {
  memory: WebAssembly.Memory;
  sim_world_new(gx: bigint, gy: bigint, hz: number): number;
  sim_world_free(handle: number): void;
  sim_step(handle: number): void;
  sim_tick(handle: number): number;
  sim_hash(handle: number): bigint;
  sim_snapshot(handle: number): number;
  sim_snapshot_byte(handle: number, index: number): number;
  sim_snapshot_set_len(handle: number, len: number): void;
  sim_snapshot_set_byte(handle: number, index: number, value: number): void;
  sim_restore(handle: number): number;
  sim_add_circle(
    handle: number,
    kind: number,
    x: bigint,
    y: bigint,
    radius: bigint,
    density: bigint,
  ): number;
  sim_add_box(
    handle: number,
    kind: number,
    x: bigint,
    y: bigint,
    hw: bigint,
    hh: bigint,
    density: bigint,
  ): number;
  sim_add_capsule(
    handle: number,
    kind: number,
    x: bigint,
    y: bigint,
    hl: bigint,
    radius: bigint,
    density: bigint,
  ): number;
  sim_remove_body(handle: number, id: number): void;
  sim_set_material(
    handle: number,
    id: number,
    friction: bigint,
    radius: bigint,
  ): void;
  sim_set_layers(
    handle: number,
    id: number,
    layer: number,
    mask: number,
    sensor: number,
  ): void;
  sim_set_velocity(
    handle: number,
    id: number,
    vx: bigint,
    vy: bigint,
    angular: bigint,
  ): void;
  sim_set_position(
    handle: number,
    id: number,
    x: bigint,
    y: bigint,
    angle: bigint,
  ): void;
  sim_apply_impulse(handle: number, id: number, ix: bigint, iy: bigint): void;
  sim_body_get(handle: number, id: number, field: number): bigint;
  sim_raycast(
    handle: number,
    x: bigint,
    y: bigint,
    dx: bigint,
    dy: bigint,
    mask: number,
  ): number;
  sim_ray_get(field: number): bigint;
  sim_kcc_new(
    handle: number,
    body: number,
    step: bigint,
    minNy: bigint,
  ): number;
  sim_kcc_move(
    handle: number,
    character: number,
    dx: bigint,
    dy: bigint,
  ): number;
  sim_golden_hash(): bigint;
  sim_nav_grid(
    handle: number,
    ox: bigint,
    oy: bigint,
    cell: bigint,
    cols: number,
    rows: number,
  ): number;
  sim_nav_load(handle: number): number;
  sim_nav_hash(handle: number): bigint;
  sim_nav_locate(handle: number, x: bigint, y: bigint): number;
  sim_nav_path(
    handle: number,
    x0: bigint,
    y0: bigint,
    x1: bigint,
    y1: bigint,
    mask: number,
  ): number;
  sim_nav_point(handle: number, index: number): bigint;
  sim_nav_raycast(
    handle: number,
    x0: bigint,
    y0: bigint,
    x1: bigint,
    y1: bigint,
  ): number;
  sim_nav_field(handle: number, x: bigint, y: bigint, mask: number): number;
  sim_agent_add(
    handle: number,
    x: bigint,
    y: bigint,
    radius: bigint,
    speed: bigint,
  ): number;
  sim_agent_remove(handle: number, id: number): void;
  sim_agent_target(handle: number, id: number, x: bigint, y: bigint): void;
  sim_agent_follow(handle: number, id: number, field: number): void;
  sim_agent_stop(handle: number, id: number): void;
  sim_agent_get(handle: number, id: number, field: number): bigint;
  sim_crowd_step(handle: number): void;
  sim_crowd_hash(handle: number): bigint;
  sim_crowd_snapshot(handle: number): number;
  sim_crowd_restore(handle: number): number;
  sim_nav_golden_hash(): bigint;
  sim_joint_distance(
    handle: number,
    first: number,
    second: number,
    ax: bigint,
    ay: bigint,
    bx: bigint,
    by: bigint,
    len: bigint,
    minLen: bigint,
  ): number;
  sim_joint_revolute(
    handle: number,
    first: number,
    second: number,
    ax: bigint,
    ay: bigint,
    bx: bigint,
    by: bigint,
    hasLimits: number,
    lower: bigint,
    upper: bigint,
    motorSpeed: bigint,
    maxTorque: bigint,
  ): number;
  sim_joint_weld(
    handle: number,
    first: number,
    second: number,
    ax: bigint,
    ay: bigint,
    bx: bigint,
    by: bigint,
    relAngle: bigint,
  ): number;
  sim_joint_remove(handle: number, id: number): number;
  sim_joint_motor(
    handle: number,
    id: number,
    speed: bigint,
    maxTorque: bigint,
  ): void;
  sim_events_len(handle: number): number;
  sim_event_get(handle: number, index: number, field: number): number;
  sim_circlecast(
    handle: number,
    x: bigint,
    y: bigint,
    dx: bigint,
    dy: bigint,
    radius: bigint,
    mask: number,
  ): number;
  sim_joint_golden_hash(): bigint;
  sim3_world_new(gx: bigint, gy: bigint, gz: bigint, hz: number): number;
  sim3_world_free(handle: number): void;
  sim3_step(handle: number): void;
  sim3_tick(handle: number): bigint;
  sim3_hash(handle: number): bigint;
  sim3_snapshot(handle: number): number;
  sim3_snapshot_byte(handle: number, index: number): number;
  sim3_snapshot_set_len(handle: number, length: number): void;
  sim3_snapshot_set_byte(handle: number, index: number, value: number): void;
  sim3_restore(handle: number): number;
  sim3_add_sphere(
    handle: number,
    kind: number,
    x: bigint,
    y: bigint,
    z: bigint,
    radius: bigint,
    density: bigint,
  ): number;
  sim3_add_box(
    handle: number,
    kind: number,
    x: bigint,
    y: bigint,
    z: bigint,
    hx: bigint,
    hy: bigint,
    hz: bigint,
    density: bigint,
  ): number;
  sim3_add_capsule(
    handle: number,
    kind: number,
    x: bigint,
    y: bigint,
    z: bigint,
    halfLen: bigint,
    radius: bigint,
    density: bigint,
  ): number;
  sim3_remove_body(handle: number, id: number): number;
  sim3_set_material(
    handle: number,
    id: number,
    friction: bigint,
    restitution: bigint,
    sensor: number,
  ): void;
  sim3_set_layers(
    handle: number,
    id: number,
    layer: number,
    mask: number,
  ): void;
  sim3_set_velocity(
    handle: number,
    id: number,
    vx: bigint,
    vy: bigint,
    vz: bigint,
    wx: bigint,
    wy: bigint,
    wz: bigint,
  ): void;
  sim3_set_position(
    handle: number,
    id: number,
    x: bigint,
    y: bigint,
    z: bigint,
  ): void;
  sim3_set_rotation(
    handle: number,
    id: number,
    qx: bigint,
    qy: bigint,
    qz: bigint,
    qw: bigint,
  ): void;
  sim3_apply_impulse(
    handle: number,
    id: number,
    ix: bigint,
    iy: bigint,
    iz: bigint,
    atPoint: number,
    px: bigint,
    py: bigint,
    pz: bigint,
  ): void;
  sim3_body_get(handle: number, id: number, field: number): bigint;
  sim3_raycast(
    handle: number,
    x: bigint,
    y: bigint,
    z: bigint,
    dx: bigint,
    dy: bigint,
    dz: bigint,
    mask: number,
  ): number;
  sim3_ray_get(field: number): bigint;
  sim3_events_len(handle: number): number;
  sim3_event_get(handle: number, index: number, field: number): number;
  sim3_golden_hash(): bigint;
  sim3_joint_ball(
    handle: number,
    first: number,
    second: number,
    ax: bigint,
    ay: bigint,
    az: bigint,
    bx: bigint,
    by: bigint,
    bz: bigint,
  ): number;
  sim3_joint_hinge(
    handle: number,
    first: number,
    second: number,
    ax: bigint,
    ay: bigint,
    az: bigint,
    bx: bigint,
    by: bigint,
    bz: bigint,
    uax: bigint,
    uay: bigint,
    uaz: bigint,
    ubx: bigint,
    uby: bigint,
    ubz: bigint,
    hasLimits: number,
    lower: bigint,
    upper: bigint,
    motorSpeed: bigint,
    maxTorque: bigint,
  ): number;
  sim3_joint_distance(
    handle: number,
    first: number,
    second: number,
    ax: bigint,
    ay: bigint,
    az: bigint,
    bx: bigint,
    by: bigint,
    bz: bigint,
    len: bigint,
    minLen: bigint,
  ): number;
  sim3_joint_remove(handle: number, id: number): number;
  sim3_joint_motor(
    handle: number,
    id: number,
    speed: bigint,
    maxTorque: bigint,
  ): void;
  sim3_hinge_angle(handle: number, id: number): bigint;
  sim3_kcc_new(
    handle: number,
    body: number,
    stepHeight: bigint,
    minGroundNy: bigint,
    snap: bigint,
  ): number;
  sim3_kcc_move(
    handle: number,
    character: number,
    dx: bigint,
    dy: bigint,
    dz: bigint,
  ): number;
  sim3_kcc_get(handle: number, character: number, field: number): bigint;
  sim3_joint_golden_hash(): bigint;
  sim3_convex_reset(handle: number): void;
  sim3_convex_vert(handle: number, x: bigint, y: bigint, z: bigint): number;
  sim3_convex_index(handle: number, index: number): void;
  sim3_convex_face_end(handle: number): number;
  sim3_add_convex(
    handle: number,
    kind: number,
    x: bigint,
    y: bigint,
    z: bigint,
    density: bigint,
  ): number;
  sim3_spherecast(
    handle: number,
    x: bigint,
    y: bigint,
    z: bigint,
    dx: bigint,
    dy: bigint,
    dz: bigint,
    radius: bigint,
    mask: number,
  ): number;
  sim3_capsulecast(
    handle: number,
    x: bigint,
    y: bigint,
    z: bigint,
    qx: bigint,
    qy: bigint,
    qz: bigint,
    qw: bigint,
    halfLen: bigint,
    radius: bigint,
    dx: bigint,
    dy: bigint,
    dz: bigint,
    mask: number,
  ): number;
  sim3_boxcast(
    handle: number,
    x: bigint,
    y: bigint,
    z: bigint,
    qx: bigint,
    qy: bigint,
    qz: bigint,
    qw: bigint,
    hx: bigint,
    hy: bigint,
    hz: bigint,
    dx: bigint,
    dy: bigint,
    dz: bigint,
    mask: number,
  ): number;
  sim3_hull_golden_hash(): bigint;
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
  private constructor(private readonly exports: SimExports) {}

  static async load(
    source: BufferSource | WebAssembly.Module | Response | Promise<Response>,
  ): Promise<Sim> {
    let module: WebAssembly.Module;
    if (source instanceof WebAssembly.Module) {
      module = source;
    } else if (source instanceof Response || source instanceof Promise) {
      module = await WebAssembly.compileStreaming(source);
    } else {
      module = await WebAssembly.compile(source);
    }
    const inst = await WebAssembly.instantiate(module, {});
    return new Sim(inst.exports as unknown as SimExports);
  }

  /** Reference scenario hash; must equal the native Rust value (`0x58d9870ba45952b5`). */
  goldenHash(): bigint {
    return BigInt.asUintN(64, this.exports.sim_golden_hash());
  }
  /** Navigation reference scenario hash; must equal the native value (`0x746c5a608abe2f26`). */
  navGoldenHash(): bigint {
    return BigInt.asUintN(64, this.exports.sim_nav_golden_hash());
  }
  /** Joint reference scenario hash; must equal the native value (`0x4bcc781a484ceeed`). */
  jointGoldenHash(): bigint {
    return BigInt.asUintN(64, this.exports.sim_joint_golden_hash());
  }
  /** 3D reference scenario hash; must equal the native value (`0x0a9c7e6fc4fa43bf`). */
  golden3Hash(): bigint {
    return BigInt.asUintN(64, this.exports.sim3_golden_hash());
  }
  /** 3D joints/character reference scenario hash (`0x5db56af63cceee85`). */
  joint3GoldenHash(): bigint {
    return BigInt.asUintN(64, this.exports.sim3_joint_golden_hash());
  }
  /** 3D convex hull reference scenario hash (`0xd656fc8f55999cf8`). */
  hull3GoldenHash(): bigint {
    return BigInt.asUintN(64, this.exports.sim3_hull_golden_hash());
  }
  /** Create a 3D world (gravity in m/s², tick rate). */
  world3(
    gravity: { x: number; y: number; z: number } = { x: 0, y: -10, z: 0 },
    tickHz = 60,
  ): Sim3World {
    return new Sim3World(
      this.exports,
      this.exports.sim3_world_new(
        fx(gravity.x),
        fx(gravity.y),
        fx(gravity.z),
        tickHz,
      ),
    );
  }

  world(gravity = { x: 0, y: -10 }, tickHz = 60): SimWorld {
    return new SimWorld(
      this.exports,
      this.exports.sim_world_new(fx(gravity.x), fx(gravity.y), tickHz),
    );
  }
}

export class SimWorld {
  constructor(
    private readonly exports: SimExports,
    readonly handle: number,
  ) {}

  free() {
    this.exports.sim_world_free(this.handle);
  }
  step() {
    this.exports.sim_step(this.handle);
  }
  get tick(): number {
    return this.exports.sim_tick(this.handle);
  }
  hash(): bigint {
    return this.exports.sim_hash(this.handle);
  }
  snapshot(): Uint8Array {
    const length = this.exports.sim_snapshot(this.handle);
    const out = new Uint8Array(length);
    for (let i = 0; i < length; i++) {
      out[i] = this.exports.sim_snapshot_byte(this.handle, i);
    }
    return out;
  }
  restore(bytes: Uint8Array): boolean {
    this.exports.sim_snapshot_set_len(this.handle, bytes.length);
    for (let i = 0; i < bytes.length; i++) {
      this.exports.sim_snapshot_set_byte(this.handle, i, bytes[i]);
    }
    return this.exports.sim_restore(this.handle) === 1;
  }

  addCircle(
    kind: BodyKind,
    x: number,
    y: number,
    radius: number,
    density = 1,
  ): number {
    return this.exports.sim_add_circle(
      this.handle,
      kind,
      fx(x),
      fx(y),
      fx(radius),
      fx(density),
    );
  }
  addBox(
    kind: BodyKind,
    x: number,
    y: number,
    halfW: number,
    halfH: number,
    density = 1,
  ): number {
    return this.exports.sim_add_box(
      this.handle,
      kind,
      fx(x),
      fx(y),
      fx(halfW),
      fx(halfH),
      fx(density),
    );
  }
  addCapsule(
    kind: BodyKind,
    x: number,
    y: number,
    halfLen: number,
    radius: number,
    density = 1,
  ): number {
    return this.exports.sim_add_capsule(
      this.handle,
      kind,
      fx(x),
      fx(y),
      fx(halfLen),
      fx(radius),
      fx(density),
    );
  }
  remove(id: number) {
    this.exports.sim_remove_body(this.handle, id);
  }
  setMaterial(id: number, friction: number, restitution: number) {
    this.exports.sim_set_material(
      this.handle,
      id,
      fx(friction),
      fx(restitution),
    );
  }
  setLayers(id: number, layer: number, mask: number, sensor = false) {
    this.exports.sim_set_layers(
      this.handle,
      id,
      layer >>> 0,
      mask >>> 0,
      sensor ? 1 : 0,
    );
  }
  setVelocity(id: number, vx: number, vy: number, angular = 0) {
    this.exports.sim_set_velocity(this.handle, id, fx(vx), fx(vy), fx(angular));
  }
  setPosition(id: number, x: number, y: number, angle = 0) {
    this.exports.sim_set_position(this.handle, id, fx(x), fx(y), fx(angle));
  }
  applyImpulse(id: number, ix: number, iy: number) {
    this.exports.sim_apply_impulse(this.handle, id, fx(ix), fx(iy));
  }
  body(id: number): BodyState {
    const get = (field: number) =>
      num(this.exports.sim_body_get(this.handle, id, field));
    return {
      x: get(0),
      y: get(1),
      angle: get(2),
      vx: get(3),
      vy: get(4),
      angularVelocity: get(5),
    };
  }
  /** Raw Q32.32 position, for hashing/networking without float round-trips. */
  bodyRaw(id: number, field: 0 | 1 | 2 | 3 | 4 | 5): Fx {
    return this.exports.sim_body_get(this.handle, id, field);
  }
  raycast(
    x: number,
    y: number,
    dx: number,
    dy: number,
    mask = 0xffffffff,
  ): RayHit | null {
    const body = this.exports.sim_raycast(
      this.handle,
      fx(x),
      fx(y),
      fx(dx),
      fx(dy),
      mask >>> 0,
    );
    if (!body) {
      return null;
    }
    const get = (field: number) => num(this.exports.sim_ray_get(field));
    return {
      body,
      fraction: get(0),
      point: { x: get(1), y: get(2) },
      normal: { x: get(3), y: get(4) },
    };
  }
  /** Character controller over a kinematic body; returns its index for `moveCharacter`. */
  character(body: number, stepHeight = 0.5, minGroundNormalY = 0.7): number {
    return this.exports.sim_kcc_new(
      this.handle,
      body,
      fx(stepHeight),
      fx(minGroundNormalY),
    );
  }
  /** Moves the controller; returns true when grounded afterwards. */
  moveCharacter(kcc: number, dx: number, dy: number): boolean {
    return this.exports.sim_kcc_move(this.handle, kcc, fx(dx), fx(dy)) === 1;
  }

  // ---- navigation (navmesh, paths, flow fields, crowd agents) ----
  private fillBuffer(bytes: Uint8Array) {
    this.exports.sim_snapshot_set_len(this.handle, bytes.length);
    for (let i = 0; i < bytes.length; i++) {
      this.exports.sim_snapshot_set_byte(this.handle, i, bytes[i]);
    }
  }
  /** Grid navmesh of cols×rows cells; `blocked` lists [col, row] cells to remove. */
  navGrid(
    origin: { x: number; y: number },
    cell: number,
    cols: number,
    rows: number,
    blocked: [number, number][] = [],
  ) {
    const buf = new Uint8Array(blocked.length * 8);
    const dv = new DataView(buf.buffer);
    blocked.forEach(([column, row], i) => {
      dv.setUint32(i * 8, column, true);
      dv.setUint32(i * 8 + 4, row, true);
    });
    this.fillBuffer(buf);
    this.exports.sim_nav_grid(
      this.handle,
      fx(origin.x),
      fx(origin.y),
      fx(cell),
      cols,
      rows,
    );
  }
  /** Load a navmesh exported by `NavMesh::to_bytes` (JSON). */
  navLoad(bytes: Uint8Array): boolean {
    this.fillBuffer(bytes);
    return this.exports.sim_nav_load(this.handle) === 1;
  }
  navHash(): bigint {
    return this.exports.sim_nav_hash(this.handle);
  }
  navLocate(x: number, y: number): number | null {
    const polygon = this.exports.sim_nav_locate(this.handle, fx(x), fx(y));
    return polygon === 0xffffffff ? null : polygon;
  }
  navPath(
    from: { x: number; y: number },
    to: { x: number; y: number },
    mask = 0xffffffff,
  ): { x: number; y: number }[] | null {
    const length = this.exports.sim_nav_path(
      this.handle,
      fx(from.x),
      fx(from.y),
      fx(to.x),
      fx(to.y),
      mask >>> 0,
    );
    if (!length) {
      return null;
    }
    const out = [];
    for (let i = 0; i < length; i++) {
      out.push({
        x: num(this.exports.sim_nav_point(this.handle, 2 * i)),
        y: num(this.exports.sim_nav_point(this.handle, 2 * i + 1)),
      });
    }
    return out;
  }
  /** Line of sight on the mesh: null when clear, else the boundary point hit. */
  navRaycast(
    from: { x: number; y: number },
    to: { x: number; y: number },
  ): { x: number; y: number } | null {
    if (
      this.exports.sim_nav_raycast(
        this.handle,
        fx(from.x),
        fx(from.y),
        fx(to.x),
        fx(to.y),
      ) !== 1
    ) {
      return null;
    }
    return {
      x: num(this.exports.sim_ray_get(1)),
      y: num(this.exports.sim_ray_get(2)),
    };
  }
  /** Flow field towards a goal shared by any number of agents; null when unreachable. */
  navField(goal: { x: number; y: number }, mask = 0xffffffff): number | null {
    const field = this.exports.sim_nav_field(
      this.handle,
      fx(goal.x),
      fx(goal.y),
      mask >>> 0,
    );
    return field === 0xffffffff ? null : field;
  }
  addAgent(x: number, y: number, radius: number, maxSpeed: number): number {
    return this.exports.sim_agent_add(
      this.handle,
      fx(x),
      fx(y),
      fx(radius),
      fx(maxSpeed),
    );
  }
  removeAgent(id: number) {
    this.exports.sim_agent_remove(this.handle, id);
  }
  agentTarget(id: number, x: number, y: number) {
    this.exports.sim_agent_target(this.handle, id, fx(x), fx(y));
  }
  agentFollow(id: number, field: number) {
    this.exports.sim_agent_follow(this.handle, id, field);
  }
  agentStop(id: number) {
    this.exports.sim_agent_stop(this.handle, id);
  }
  agent(id: number): {
    x: number;
    y: number;
    vx: number;
    vy: number;
    arrived: boolean;
    poly: number;
  } {
    const get = (field: number) =>
      this.exports.sim_agent_get(this.handle, id, field);
    return {
      x: num(get(0)),
      y: num(get(1)),
      vx: num(get(2)),
      vy: num(get(3)),
      arrived: get(4) === 1n,
      poly: Number(get(5)),
    };
  }
  /** Advance all agents one tick (call next to `step()`). */
  crowdStep() {
    this.exports.sim_crowd_step(this.handle);
  }
  crowdHash(): bigint {
    return this.exports.sim_crowd_hash(this.handle);
  }
  crowdSnapshot(): Uint8Array {
    const length = this.exports.sim_crowd_snapshot(this.handle);
    const out = new Uint8Array(length);
    for (let i = 0; i < length; i++) {
      out[i] = this.exports.sim_snapshot_byte(this.handle, i);
    }
    return out;
  }
  crowdRestore(bytes: Uint8Array): boolean {
    this.fillBuffer(bytes);
    return this.exports.sim_crowd_restore(this.handle) === 1;
  }

  // ---- joints, contact events, shape casts ----
  /** Distance joint between local anchors; `minLength < length` allows slack (rope). */
  jointDistance(
    first: number,
    second: number,
    anchorA: { x: number; y: number },
    anchorB: { x: number; y: number },
    length: number,
    minLength = length,
  ): number {
    return this.exports.sim_joint_distance(
      this.handle,
      first,
      second,
      fx(anchorA.x),
      fx(anchorA.y),
      fx(anchorB.x),
      fx(anchorB.y),
      fx(length),
      fx(minLength),
    );
  }
  /** Pin joint; optional angle limits and motor (`maxTorque > 0` enables it). */
  jointRevolute(
    first: number,
    second: number,
    anchorA: { x: number; y: number },
    anchorB: { x: number; y: number },
    opts: {
      lower?: number;
      upper?: number;
      motorSpeed?: number;
      maxTorque?: number;
    } = {},
  ): number {
    const limits = opts.lower !== undefined && opts.upper !== undefined;
    return this.exports.sim_joint_revolute(
      this.handle,
      first,
      second,
      fx(anchorA.x),
      fx(anchorA.y),
      fx(anchorB.x),
      fx(anchorB.y),
      limits ? 1 : 0,
      fx(opts.lower ?? 0),
      fx(opts.upper ?? 0),
      fx(opts.motorSpeed ?? 0),
      fx(opts.maxTorque ?? 0),
    );
  }
  jointWeld(
    first: number,
    second: number,
    anchorA: { x: number; y: number },
    anchorB: { x: number; y: number },
    relAngle = 0,
  ): number {
    return this.exports.sim_joint_weld(
      this.handle,
      first,
      second,
      fx(anchorA.x),
      fx(anchorA.y),
      fx(anchorB.x),
      fx(anchorB.y),
      fx(relAngle),
    );
  }
  removeJoint(id: number): boolean {
    return this.exports.sim_joint_remove(this.handle, id) === 1;
  }
  setMotor(joint: number, speed: number, maxTorque: number) {
    this.exports.sim_joint_motor(this.handle, joint, fx(speed), fx(maxTorque));
  }
  /** Contact begin/end events from the last `step()`. */
  events(): { a: number; b: number; begin: boolean }[] {
    const length = this.exports.sim_events_len(this.handle);
    const out = [];
    for (let i = 0; i < length; i++) {
      out.push({
        a: this.exports.sim_event_get(this.handle, i, 0),
        b: this.exports.sim_event_get(this.handle, i, 1),
        begin: this.exports.sim_event_get(this.handle, i, 2) === 1,
      });
    }
    return out;
  }
  /** Sweep a circle along `dir`; same result shape as `raycast()`. */
  circleCast(
    from: { x: number; y: number },
    dir: { x: number; y: number },
    radius: number,
    mask = 0xffffffff,
  ) {
    const hit = this.exports.sim_circlecast(
      this.handle,
      fx(from.x),
      fx(from.y),
      fx(dir.x),
      fx(dir.y),
      fx(radius),
      mask >>> 0,
    );
    if (!hit) {
      return null;
    }
    const get = (field: number) => num(this.exports.sim_ray_get(field));
    return {
      body: hit - 1,
      fraction: get(0),
      point: { x: get(1), y: get(2) },
      normal: { x: get(3), y: get(4) },
    };
  }
}

export type Vec3 = { x: number; y: number; z: number };
export type Quat = { x: number; y: number; z: number; w: number };
export type Body3State = {
  position: Vec3;
  rotation: Quat;
  velocity: Vec3;
  angularVelocity: Vec3;
};
export type Ray3Hit = {
  body: number;
  fraction: number;
  point: Vec3;
  normal: Vec3;
};

/** Deterministic 3D world (spheres, boxes, capsules) over the same wasm module. */
export class Sim3World {
  constructor(
    private readonly exports: SimExports,
    readonly handle: number,
  ) {}
  free() {
    this.exports.sim3_world_free(this.handle);
  }
  step() {
    this.exports.sim3_step(this.handle);
  }
  tick(): number {
    return Number(this.exports.sim3_tick(this.handle));
  }
  hash(): bigint {
    return this.exports.sim3_hash(this.handle);
  }
  snapshot(): Uint8Array {
    const length = this.exports.sim3_snapshot(this.handle);
    const out = new Uint8Array(length);
    for (let i = 0; i < length; i++) {
      out[i] = this.exports.sim3_snapshot_byte(this.handle, i);
    }
    return out;
  }
  restore(bytes: Uint8Array): boolean {
    this.exports.sim3_snapshot_set_len(this.handle, bytes.length);
    for (let i = 0; i < bytes.length; i++) {
      this.exports.sim3_snapshot_set_byte(this.handle, i, bytes[i]);
    }
    return this.exports.sim3_restore(this.handle) === 1;
  }
  addSphere(
    kind: BodyKind,
    position: Vec3,
    radius: number,
    density = 1,
  ): number {
    return this.exports.sim3_add_sphere(
      this.handle,
      kind,
      fx(position.x),
      fx(position.y),
      fx(position.z),
      fx(radius),
      fx(density),
    );
  }
  addBox(kind: BodyKind, position: Vec3, half: Vec3, density = 1): number {
    return this.exports.sim3_add_box(
      this.handle,
      kind,
      fx(position.x),
      fx(position.y),
      fx(position.z),
      fx(half.x),
      fx(half.y),
      fx(half.z),
      fx(density),
    );
  }
  /** Capsule along the body y axis. */
  addCapsule(
    kind: BodyKind,
    position: Vec3,
    halfLen: number,
    radius: number,
    density = 1,
  ): number {
    return this.exports.sim3_add_capsule(
      this.handle,
      kind,
      fx(position.x),
      fx(position.y),
      fx(position.z),
      fx(halfLen),
      fx(radius),
      fx(density),
    );
  }
  remove(id: number): boolean {
    return this.exports.sim3_remove_body(this.handle, id) === 1;
  }
  setMaterial(
    id: number,
    friction: number,
    restitution: number,
    sensor = false,
  ) {
    this.exports.sim3_set_material(
      this.handle,
      id,
      fx(friction),
      fx(restitution),
      sensor ? 1 : 0,
    );
  }
  setLayers(id: number, layer: number, mask: number) {
    this.exports.sim3_set_layers(this.handle, id, layer >>> 0, mask >>> 0);
  }
  setVelocity(id: number, linear: Vec3, angular: Vec3 = { x: 0, y: 0, z: 0 }) {
    this.exports.sim3_set_velocity(
      this.handle,
      id,
      fx(linear.x),
      fx(linear.y),
      fx(linear.z),
      fx(angular.x),
      fx(angular.y),
      fx(angular.z),
    );
  }
  setPosition(id: number, position: Vec3) {
    this.exports.sim3_set_position(
      this.handle,
      id,
      fx(position.x),
      fx(position.y),
      fx(position.z),
    );
  }
  setRotation(id: number, rotation: Quat) {
    this.exports.sim3_set_rotation(
      this.handle,
      id,
      fx(rotation.x),
      fx(rotation.y),
      fx(rotation.z),
      fx(rotation.w),
    );
  }
  /** Axis-angle helper for `setRotation` (axis must be unit length). */
  static axisAngle(axis: Vec3, angle: number): Quat {
    const sine = Math.sin(angle / 2);
    return {
      x: axis.x * sine,
      y: axis.y * sine,
      z: axis.z * sine,
      w: Math.cos(angle / 2),
    };
  }
  applyImpulse(id: number, impulse: Vec3, at?: Vec3) {
    this.exports.sim3_apply_impulse(
      this.handle,
      id,
      fx(impulse.x),
      fx(impulse.y),
      fx(impulse.z),
      at ? 1 : 0,
      fx(at?.x ?? 0),
      fx(at?.y ?? 0),
      fx(at?.z ?? 0),
    );
  }
  body(id: number): Body3State {
    const get = (field: number) =>
      num(this.exports.sim3_body_get(this.handle, id, field));
    return {
      position: { x: get(0), y: get(1), z: get(2) },
      rotation: { x: get(3), y: get(4), z: get(5), w: get(6) },
      velocity: { x: get(7), y: get(8), z: get(9) },
      angularVelocity: { x: get(10), y: get(11), z: get(12) },
    };
  }
  raycast(from: Vec3, dir: Vec3, mask = 0xffffffff): Ray3Hit | null {
    const hit = this.exports.sim3_raycast(
      this.handle,
      fx(from.x),
      fx(from.y),
      fx(from.z),
      fx(dir.x),
      fx(dir.y),
      fx(dir.z),
      mask >>> 0,
    );
    if (!hit) {
      return null;
    }
    const get = (field: number) => num(this.exports.sim3_ray_get(field));
    return {
      body: hit - 1,
      fraction: get(0),
      point: { x: get(1), y: get(2), z: get(3) },
      normal: { x: get(4), y: get(5), z: get(6) },
    };
  }
  events(): { a: number; b: number; begin: boolean }[] {
    const length = this.exports.sim3_events_len(this.handle);
    const out = [];
    for (let i = 0; i < length; i++) {
      out.push({
        a: this.exports.sim3_event_get(this.handle, i, 0),
        b: this.exports.sim3_event_get(this.handle, i, 1),
        begin: this.exports.sim3_event_get(this.handle, i, 2) === 1,
      });
    }
    return out;
  }

  // ---- joints and character controller ----
  jointBall(
    first: number,
    second: number,
    anchorA: Vec3,
    anchorB: Vec3,
  ): number {
    return this.exports.sim3_joint_ball(
      this.handle,
      first,
      second,
      fx(anchorA.x),
      fx(anchorA.y),
      fx(anchorA.z),
      fx(anchorB.x),
      fx(anchorB.y),
      fx(anchorB.z),
    );
  }
  /** Hinge around local axes; optional angle limits (radians) and motor (`maxTorque > 0`). */
  jointHinge(
    first: number,
    second: number,
    anchorA: Vec3,
    anchorB: Vec3,
    axisA: Vec3,
    axisB: Vec3,
    opts: {
      lower?: number;
      upper?: number;
      motorSpeed?: number;
      maxTorque?: number;
    } = {},
  ): number {
    const limits = opts.lower !== undefined && opts.upper !== undefined;
    return this.exports.sim3_joint_hinge(
      this.handle,
      first,
      second,
      fx(anchorA.x),
      fx(anchorA.y),
      fx(anchorA.z),
      fx(anchorB.x),
      fx(anchorB.y),
      fx(anchorB.z),
      fx(axisA.x),
      fx(axisA.y),
      fx(axisA.z),
      fx(axisB.x),
      fx(axisB.y),
      fx(axisB.z),
      limits ? 1 : 0,
      fx(opts.lower ?? 0),
      fx(opts.upper ?? 0),
      fx(opts.motorSpeed ?? 0),
      fx(opts.maxTorque ?? 0),
    );
  }
  /** Distance joint; `minLength < length` allows slack (rope). */
  jointDistance(
    first: number,
    second: number,
    anchorA: Vec3,
    anchorB: Vec3,
    length: number,
    minLength = length,
  ): number {
    return this.exports.sim3_joint_distance(
      this.handle,
      first,
      second,
      fx(anchorA.x),
      fx(anchorA.y),
      fx(anchorA.z),
      fx(anchorB.x),
      fx(anchorB.y),
      fx(anchorB.z),
      fx(length),
      fx(minLength),
    );
  }
  removeJoint(id: number): boolean {
    return this.exports.sim3_joint_remove(this.handle, id) === 1;
  }
  setMotor(joint: number, speed: number, maxTorque: number) {
    this.exports.sim3_joint_motor(this.handle, joint, fx(speed), fx(maxTorque));
  }
  hingeAngle(joint: number): number {
    return num(this.exports.sim3_hinge_angle(this.handle, joint));
  }
  /** Capsule character controller over a kinematic capsule body; returns a controller index. */
  character(
    body: number,
    stepHeight = 0.5,
    minGroundNormalY = 0.7,
    snapDistance = 0.25,
  ): number {
    return this.exports.sim3_kcc_new(
      this.handle,
      body,
      fx(stepHeight),
      fx(minGroundNormalY),
      fx(snapDistance),
    );
  }
  /** Move the controller this tick (call before `step()`); returns whether it stands on ground. */
  moveCharacter(character: number, delta: Vec3): boolean {
    return (
      this.exports.sim3_kcc_move(
        this.handle,
        character,
        fx(delta.x),
        fx(delta.y),
        fx(delta.z),
      ) === 1
    );
  }
  characterState(character: number): {
    groundNormal: Vec3;
    velocity: Vec3;
    grounded: boolean;
    groundBody: number | null;
  } {
    const get = (field: number) =>
      this.exports.sim3_kcc_get(this.handle, character, field);
    const gb = Number(get(7));
    return {
      groundNormal: { x: num(get(0)), y: num(get(1)), z: num(get(2)) },
      velocity: { x: num(get(3)), y: num(get(4)), z: num(get(5)) },
      grounded: get(6) === 1n,
      groundBody: gb ? gb - 1 : null,
    };
  }

  // ---- convex hulls and sweeps ----
  /** Convex polyhedron from body-space vertices (<= 32) and faces as index loops (any winding);
   *  author vertices around the centre of mass. Returns the body id (throws on an invalid hull). */
  addConvex(
    kind: BodyKind,
    pos: Vec3,
    verts: Vec3[],
    faces: number[][],
    density = 1,
  ): number {
    this.exports.sim3_convex_reset(this.handle);
    for (const vertex of verts) {
      this.exports.sim3_convex_vert(
        this.handle,
        fx(vertex.x),
        fx(vertex.y),
        fx(vertex.z),
      );
    }
    for (const face of faces) {
      for (const index of face) {
        this.exports.sim3_convex_index(this.handle, index);
      }
      this.exports.sim3_convex_face_end(this.handle);
    }
    const id = this.exports.sim3_add_convex(
      this.handle,
      kind,
      fx(pos.x),
      fx(pos.y),
      fx(pos.z),
      fx(density),
    );
    if (id >>> 0 === 0xffffffff) {
      throw new Error(
        "sim3: invalid convex hull (need >= 4 vertices and >= 4 faces)",
      );
    }
    return id;
  }
  /** Sweep a sphere along `dir`; `fraction` is where the centre stops, `point`/`normal` lie on the hit surface. */
  sphereCast(
    from: Vec3,
    dir: Vec3,
    radius: number,
    mask = 0xffffffff,
  ): Ray3Hit | null {
    const id = this.exports.sim3_spherecast(
      this.handle,
      fx(from.x),
      fx(from.y),
      fx(from.z),
      fx(dir.x),
      fx(dir.y),
      fx(dir.z),
      fx(radius),
      mask,
    );
    if (id === 0) {
      return null;
    }
    const get = (field: number) => num(this.exports.sim3_ray_get(field));
    return {
      body: id - 1,
      fraction: get(0),
      point: { x: get(1), y: get(2), z: get(3) },
      normal: { x: get(4), y: get(5), z: get(6) },
    };
  }
  /** Sweep a capsule (segment along its local y, rotated by `rot`) along `dir`; same result shape as `sphereCast`. */
  capsuleCast(
    from: Vec3,
    rot: Quat,
    halfLen: number,
    radius: number,
    dir: Vec3,
    mask = 0xffffffff,
  ): Ray3Hit | null {
    const id = this.exports.sim3_capsulecast(
      this.handle,
      fx(from.x),
      fx(from.y),
      fx(from.z),
      fx(rot.x),
      fx(rot.y),
      fx(rot.z),
      fx(rot.w),
      fx(halfLen),
      fx(radius),
      fx(dir.x),
      fx(dir.y),
      fx(dir.z),
      mask,
    );
    return this.rayResult(id);
  }
  /** Sweep a box (`half` extents, rotated by `rot`) along `dir`; same result shape as `sphereCast`. */
  boxCast(
    from: Vec3,
    rot: Quat,
    half: Vec3,
    dir: Vec3,
    mask = 0xffffffff,
  ): Ray3Hit | null {
    const id = this.exports.sim3_boxcast(
      this.handle,
      fx(from.x),
      fx(from.y),
      fx(from.z),
      fx(rot.x),
      fx(rot.y),
      fx(rot.z),
      fx(rot.w),
      fx(half.x),
      fx(half.y),
      fx(half.z),
      fx(dir.x),
      fx(dir.y),
      fx(dir.z),
      mask,
    );
    return this.rayResult(id);
  }
  private rayResult(id: number): Ray3Hit | null {
    if (id === 0) {
      return null;
    }
    const get = (field: number) => num(this.exports.sim3_ray_get(field));
    return {
      body: id - 1,
      fraction: get(0),
      point: { x: get(1), y: get(2), z: get(3) },
      normal: { x: get(4), y: get(5), z: get(6) },
    };
  }
}
