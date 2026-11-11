//! Server-side game logic for sync rooms, running inside a WebAssembly sandbox.
//!
//! A project uploads a `.wasm` module (any language; the ABI is plain integers/floats) and
//! references it from room props as `{"logic": "<name>"}`. The node instantiates one
//! instance per room with a fuel budget per call and a memory cap, so a buggy or hostile
//! module can only burn its own budget — never stall the node. Guest memory is *not*
//! persisted across node failover; keep game state in components (they are checkpointed).
//!
//! Guest exports (functions the module provides):
//! ```text
//! memory                                   linear memory
//! aurix_init(seed: i64)                    once per instance (and after a failover restore)
//! aurix_join(actor: i32) -> i64            optional: return the actor's avatar entity (0 = none)
//! aurix_leave(actor: i32, entity: i64)     optional
//! aurix_step()                             once per tick
//! ```
//! Host imports (module `"aurix"`): entity handles are i64 (0 = none/invalid), actors i32
//! (0 = server / everyone), component ids from `component_id`, fields by index in the schema:
//! ```text
//! dt() -> f32                tick() -> i64              rand() -> i32
//! spawn(owner: i32) -> i64   despawn(id: i64) -> i32    entity_owner(id) -> i32
//! entity_count() -> i32      entity_at(index: i32) -> i64
//! component_id(name_ptr, name_len) -> i32  (-1 = unknown)   has_component(id, comp) -> i32
//! remove_component(id, comp) -> i32
//! get_f32(id, comp, field) -> f32          set_f32(id, comp, field, v) -> i32
//! get_i64(id, comp, field) -> i64          set_i64(id, comp, field, v) -> i32
//! get_vec3(id, comp, field, out_ptr) -> i32 (writes 3 f32)   set_vec3(id, comp, field, x, y, z) -> i32
//! set_bytes(id, comp, field, ptr, len) -> i32   (string / bytes fields)
//! input_count() -> i32       input_actor(i) -> i32      input_entity(i) -> i64
//! input_fresh(i) -> i32      input_read(i, ptr, cap) -> i32 (bytes copied)
//! raise(code: i32, target_actor: i32, ptr, len)    event to clients (0 = everyone)
//! hitbox(id, cx, cy, cz, hx, hy, hz)               register a lag-compensation box for this tick
//! log(level: i32, ptr, len)                         0 error, 1 warn, 2 info, 3 debug (rate limited)
//! ```
//! `set_*` on a missing component creates it with zero defaults first.

use net_common::types::ActorId;
use net_lagcomp::Hitbox;
use net_sim::{EntityId, Frame};
use net_wire::schema::FieldType;
use net_wire::value::{ComponentValue, Value};
use parking_lot::Mutex;
use std::collections::HashMap;
use wasmtime::*;

/// Fuel (roughly instructions) a single guest call may burn before it is trapped.
pub const FUEL_PER_CALL: u64 = 50_000_000;
pub const MAX_MEMORY_BYTES: usize = 64 << 20;
pub const MAX_MODULE_BYTES: usize = 8 << 20;
/// Guest log lines per second per room before the rest is dropped.
pub const LOG_BUDGET_PER_TICK: u32 = 4;

#[derive(Debug, thiserror::Error)]
pub enum GameError {
    #[error("module too large ({0} bytes, max {MAX_MODULE_BYTES})")]
    TooLarge(usize),
    #[error("invalid module: {0}")]
    Invalid(String),
    #[error("module must export {0}")]
    MissingExport(&'static str),
    #[error("instantiate: {0}")]
    Instantiate(String),
}

/// One compiled-module cache per node (compilation is the expensive part; instances are cheap).
pub struct GameEngine {
    engine: Engine,
    cache: Mutex<HashMap<String, Module>>,
}

impl Default for GameEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl GameEngine {
    pub fn new() -> Self {
        let mut cfg = Config::new();
        cfg.consume_fuel(true);
        cfg.cranelift_opt_level(OptLevel::Speed);
        // One compile thread: a module is compiled once per node, and a parallel compile burst
        // leaves tens of MB of allocator high-water mark on every worker thread.
        cfg.parallel_compilation(false);
        let engine = Engine::new(&cfg).expect("wasmtime engine");
        Self {
            engine,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn hash(bytes: &[u8]) -> String {
        blake3::hash(bytes).to_hex().to_string()
    }

    /// Compiles and validates a module (exports). Cached by content hash.
    pub fn load(&self, bytes: &[u8]) -> Result<(String, Module), GameError> {
        if bytes.len() > MAX_MODULE_BYTES {
            return Err(GameError::TooLarge(bytes.len()));
        }
        let hash = Self::hash(bytes);
        if let Some(m) = self.cache.lock().get(&hash) {
            return Ok((hash, m.clone()));
        }
        let module =
            Module::new(&self.engine, bytes).map_err(|e| GameError::Invalid(e.to_string()))?;
        let has = |name: &str, kind_ok: &dyn Fn(&ExternType) -> bool| {
            module
                .exports()
                .any(|e| e.name() == name && kind_ok(&e.ty()))
        };
        if !has("memory", &|t| matches!(t, ExternType::Memory(_))) {
            return Err(GameError::MissingExport("memory"));
        }
        for f in ["aurix_init", "aurix_step"] {
            if !has(f, &|t| matches!(t, ExternType::Func(_))) {
                return Err(GameError::MissingExport(if f == "aurix_init" {
                    "aurix_init"
                } else {
                    "aurix_step"
                }));
            }
        }
        self.cache.lock().insert(hash.clone(), module.clone());
        Ok((hash, module))
    }

    pub fn instantiate(&self, module: &Module, seed: u64) -> Result<WasmLogic, GameError> {
        WasmLogic::new(&self.engine, module, seed)
    }
}

pub type StepInput = (ActorId, Option<EntityId>, Vec<u8>, bool);

struct Host {
    /// Valid only while a guest call is in flight (set by `WasmLogic::call`).
    frame: *mut Frame,
    inputs: Vec<StepInput>,
    hitboxes: Vec<(EntityId, Hitbox)>,
    entities: Vec<EntityId>,
    limits: StoreLimits,
    log_budget: u32,
}

// The raw pointer is only dereferenced on the thread that runs the guest call, which holds
// `&mut Frame` for the whole call.
unsafe impl Send for Host {}

fn frame<'a>(c: &mut Caller<'a, Host>) -> Option<&'a mut Frame> {
    // SAFETY: see `Host::frame`; null outside of a call.
    unsafe { c.data().frame.as_mut() }
}

/// Guest-visible handle: `EntityId::to_u64() + 1`, so 0 means "no entity".
fn handle(e: EntityId) -> i64 {
    (e.to_u64() + 1) as i64
}

fn ent(id: i64) -> EntityId {
    EntityId::from_u64((id as u64).wrapping_sub(1))
}

fn actor(a: i32) -> Option<ActorId> {
    (a > 0 && a <= u16::MAX as i32).then_some(ActorId(a as u16))
}

fn memory(c: &mut Caller<'_, Host>) -> Option<Memory> {
    c.get_export("memory").and_then(|e| e.into_memory())
}

fn read_bytes(c: &mut Caller<'_, Host>, ptr: i32, len: i32) -> Option<Vec<u8>> {
    let m = memory(c)?;
    let (p, l) = (ptr as usize, len as usize);
    m.data(c).get(p..p.checked_add(l)?).map(|s| s.to_vec())
}

fn write_bytes(c: &mut Caller<'_, Host>, ptr: i32, data: &[u8]) -> bool {
    let Some(m) = memory(c) else { return false };
    let p = ptr as usize;
    match m.data_mut(c).get_mut(p..p + data.len()) {
        Some(dst) => {
            dst.copy_from_slice(data);
            true
        }
        None => false,
    }
}

/// Field access helpers shared by get_*/set_*.
fn field_mut(f: &mut Frame, id: i64, comp: i32, field: i32) -> Option<&mut Value> {
    let c = f.schema.by_id(u16::try_from(comp).ok()?)?.clone();
    let fi = usize::try_from(field).ok()?;
    if fi >= c.fields.len() {
        return None;
    }
    if f.get(ent(id), &c.name).is_none() {
        f.set(ent(id), &c.name, ComponentValue::zero(&c)).ok()?;
    }
    f.get_mut(ent(id), &c.name)?.fields.get_mut(fi)
}

fn field_ref(f: &Frame, id: i64, comp: i32, field: i32) -> Option<(&Value, &FieldType)> {
    let c = f.schema.by_id(u16::try_from(comp).ok()?)?;
    let fi = usize::try_from(field).ok()?;
    let v = f.get(ent(id), &c.name)?.fields.get(fi)?;
    Some((v, &c.fields[fi].ty))
}

pub struct WasmLogic {
    store: Store<Host>,
    init: TypedFunc<i64, ()>,
    step: TypedFunc<(), ()>,
    join: Option<TypedFunc<i32, i64>>,
    leave: Option<TypedFunc<(i32, i64), ()>>,
    seed: u64,
    initialized: bool,
    /// Set after a trap: the module is disabled for this room (logged once).
    pub dead: bool,
}

impl WasmLogic {
    fn new(engine: &Engine, module: &Module, seed: u64) -> Result<Self, GameError> {
        let mut linker: Linker<Host> = Linker::new(engine);
        Self::link(&mut linker).map_err(|e| GameError::Instantiate(e.to_string()))?;
        let host = Host {
            frame: std::ptr::null_mut(),
            inputs: Vec::new(),
            hitboxes: Vec::new(),
            entities: Vec::new(),
            limits: StoreLimitsBuilder::new()
                .memory_size(MAX_MEMORY_BYTES)
                .instances(1)
                .tables(4)
                .build(),
            log_budget: LOG_BUDGET_PER_TICK,
        };
        let mut store = Store::new(engine, host);
        store.limiter(|h| &mut h.limits);
        store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|e| GameError::Instantiate(e.to_string()))?;
        let instance = linker
            .instantiate(&mut store, module)
            .map_err(|e| GameError::Instantiate(e.to_string()))?;
        let init = instance
            .get_typed_func::<i64, ()>(&mut store, "aurix_init")
            .map_err(|e| GameError::Instantiate(e.to_string()))?;
        let step = instance
            .get_typed_func::<(), ()>(&mut store, "aurix_step")
            .map_err(|e| GameError::Instantiate(e.to_string()))?;
        let join = instance
            .get_typed_func::<i32, i64>(&mut store, "aurix_join")
            .ok();
        let leave = instance
            .get_typed_func::<(i32, i64), ()>(&mut store, "aurix_leave")
            .ok();
        Ok(Self {
            store,
            init,
            step,
            join,
            leave,
            seed,
            initialized: false,
            dead: false,
        })
    }

    fn link(l: &mut Linker<Host>) -> wasmtime::Result<()> {
        l.func_wrap("aurix", "dt", |mut c: Caller<'_, Host>| -> f32 {
            frame(&mut c).map(|f| f.dt()).unwrap_or(0.0)
        })?;
        l.func_wrap("aurix", "tick", |mut c: Caller<'_, Host>| -> i64 {
            frame(&mut c).map(|f| f.tick.0 as i64).unwrap_or(0)
        })?;
        l.func_wrap("aurix", "rand", |mut c: Caller<'_, Host>| -> i32 {
            frame(&mut c).map(|f| f.rand_u32() as i32).unwrap_or(0)
        })?;
        l.func_wrap(
            "aurix",
            "spawn",
            |mut c: Caller<'_, Host>, owner: i32| -> i64 {
                frame(&mut c)
                    .and_then(|f| f.spawn(actor(owner)).ok())
                    .map(handle)
                    .unwrap_or(0)
            },
        )?;
        l.func_wrap(
            "aurix",
            "despawn",
            |mut c: Caller<'_, Host>, id: i64| -> i32 {
                frame(&mut c).is_some_and(|f| f.despawn(ent(id)).is_ok()) as i32
            },
        )?;
        l.func_wrap(
            "aurix",
            "entity_owner",
            |mut c: Caller<'_, Host>, id: i64| -> i32 {
                frame(&mut c)
                    .and_then(|f| f.entity(ent(id)).and_then(|e| e.owner))
                    .map(|a| a.0 as i32)
                    .unwrap_or(0)
            },
        )?;
        l.func_wrap("aurix", "entity_count", |mut c: Caller<'_, Host>| -> i32 {
            let ids: Vec<EntityId> = frame(&mut c)
                .map(|f| f.entities().map(|e| e.id).collect())
                .unwrap_or_default();
            let n = ids.len() as i32;
            c.data_mut().entities = ids;
            n
        })?;
        l.func_wrap("aurix", "entity_at", |c: Caller<'_, Host>, i: i32| -> i64 {
            usize::try_from(i)
                .ok()
                .and_then(|i| c.data().entities.get(i))
                .map(|e| handle(*e))
                .unwrap_or(0)
        })?;
        l.func_wrap(
            "aurix",
            "component_id",
            |mut c: Caller<'_, Host>, ptr: i32, len: i32| -> i32 {
                let Some(name) = read_bytes(&mut c, ptr, len) else {
                    return -1;
                };
                let Ok(name) = std::str::from_utf8(&name) else {
                    return -1;
                };
                frame(&mut c)
                    .and_then(|f| f.schema.component(name))
                    .map(|cmp| cmp.id as i32)
                    .unwrap_or(-1)
            },
        )?;
        l.func_wrap(
            "aurix",
            "has_component",
            |mut c: Caller<'_, Host>, id: i64, comp: i32| -> i32 {
                frame(&mut c).is_some_and(|f| {
                    f.schema
                        .by_id(comp as u16)
                        .is_some_and(|cmp| f.get(ent(id), &cmp.name).is_some())
                }) as i32
            },
        )?;
        l.func_wrap(
            "aurix",
            "remove_component",
            |mut c: Caller<'_, Host>, id: i64, comp: i32| -> i32 {
                frame(&mut c).is_some_and(|f| {
                    let Some(cmp) = f.schema.by_id(comp as u16).cloned() else {
                        return false;
                    };
                    f.remove_component(ent(id), &cmp.name).is_ok()
                }) as i32
            },
        )?;
        l.func_wrap(
            "aurix",
            "get_f32",
            |mut c: Caller<'_, Host>, id: i64, comp: i32, field: i32| -> f32 {
                frame(&mut c)
                    .and_then(|f| field_ref(f, id, comp, field))
                    .map(|(v, _)| match v {
                        Value::F32(x) => *x,
                        Value::Int(x) => *x as f32,
                        Value::UInt(x) => *x as f32,
                        Value::Bool(b) => *b as u8 as f32,
                        _ => 0.0,
                    })
                    .unwrap_or(0.0)
            },
        )?;
        l.func_wrap(
            "aurix",
            "set_f32",
            |mut c: Caller<'_, Host>, id: i64, comp: i32, field: i32, v: f32| -> i32 {
                frame(&mut c).is_some_and(|f| {
                    let Some(slot) = field_mut(f, id, comp, field) else {
                        return false;
                    };
                    match slot {
                        Value::F32(x) => *x = v,
                        Value::Int(x) => *x = v as i64,
                        Value::UInt(x) => *x = v.max(0.0) as u64,
                        Value::Bool(b) => *b = v != 0.0,
                        _ => return false,
                    }
                    true
                }) as i32
            },
        )?;
        l.func_wrap(
            "aurix",
            "get_i64",
            |mut c: Caller<'_, Host>, id: i64, comp: i32, field: i32| -> i64 {
                frame(&mut c)
                    .and_then(|f| field_ref(f, id, comp, field))
                    .map(|(v, _)| match v {
                        Value::Int(x) => *x,
                        Value::UInt(x) => *x as i64,
                        Value::F32(x) => *x as i64,
                        Value::Bool(b) => *b as i64,
                        _ => 0,
                    })
                    .unwrap_or(0)
            },
        )?;
        l.func_wrap(
            "aurix",
            "set_i64",
            |mut c: Caller<'_, Host>, id: i64, comp: i32, field: i32, v: i64| -> i32 {
                frame(&mut c).is_some_and(|f| {
                    let Some(slot) = field_mut(f, id, comp, field) else {
                        return false;
                    };
                    match slot {
                        Value::Int(x) => *x = v,
                        Value::UInt(x) => *x = v.max(0) as u64,
                        Value::F32(x) => *x = v as f32,
                        Value::Bool(b) => *b = v != 0,
                        _ => return false,
                    }
                    true
                }) as i32
            },
        )?;
        l.func_wrap(
            "aurix",
            "get_vec3",
            |mut c: Caller<'_, Host>, id: i64, comp: i32, field: i32, out: i32| -> i32 {
                let v = frame(&mut c)
                    .and_then(|f| field_ref(f, id, comp, field))
                    .and_then(|(v, _)| match v {
                        Value::Vec3(a) => Some(*a),
                        Value::Vec2(a) => Some([a[0], a[1], 0.0]),
                        Value::Quat(q) => Some([q[0], q[1], q[2]]),
                        _ => None,
                    });
                let Some(v) = v else { return 0 };
                let mut buf = [0u8; 12];
                for (i, x) in v.iter().enumerate() {
                    buf[i * 4..i * 4 + 4].copy_from_slice(&x.to_le_bytes());
                }
                write_bytes(&mut c, out, &buf) as i32
            },
        )?;
        l.func_wrap(
            "aurix",
            "set_vec3",
            |mut c: Caller<'_, Host>,
             id: i64,
             comp: i32,
             field: i32,
             x: f32,
             y: f32,
             z: f32|
             -> i32 {
                frame(&mut c).is_some_and(|f| {
                    let Some(slot) = field_mut(f, id, comp, field) else {
                        return false;
                    };
                    match slot {
                        Value::Vec3(a) => *a = [x, y, z],
                        Value::Vec2(a) => *a = [x, y],
                        _ => return false,
                    }
                    true
                }) as i32
            },
        )?;
        l.func_wrap(
            "aurix",
            "set_bytes",
            |mut c: Caller<'_, Host>, id: i64, comp: i32, field: i32, ptr: i32, len: i32| -> i32 {
                let Some(data) = read_bytes(&mut c, ptr, len) else {
                    return 0;
                };
                frame(&mut c).is_some_and(|f| {
                    let max = field_ref(f, id, comp, field)
                        .map(|(_, t)| match t {
                            FieldType::String { max } | FieldType::Bytes { max } => *max as usize,
                            _ => 0,
                        })
                        .unwrap_or(usize::MAX);
                    let Some(slot) = field_mut(f, id, comp, field) else {
                        return false;
                    };
                    match slot {
                        Value::String(s) => match std::str::from_utf8(&data) {
                            Ok(t) if t.len() <= max => *s = t.to_string(),
                            _ => return false,
                        },
                        Value::Bytes(b) if data.len() <= max => *b = data,
                        _ => return false,
                    }
                    true
                }) as i32
            },
        )?;
        l.func_wrap("aurix", "input_count", |c: Caller<'_, Host>| -> i32 {
            c.data().inputs.len() as i32
        })?;
        l.func_wrap(
            "aurix",
            "input_actor",
            |c: Caller<'_, Host>, i: i32| -> i32 {
                c.data()
                    .inputs
                    .get(i as usize)
                    .map(|x| x.0 .0 as i32)
                    .unwrap_or(0)
            },
        )?;
        l.func_wrap(
            "aurix",
            "input_entity",
            |c: Caller<'_, Host>, i: i32| -> i64 {
                c.data()
                    .inputs
                    .get(i as usize)
                    .and_then(|x| x.1)
                    .map(handle)
                    .unwrap_or(0)
            },
        )?;
        l.func_wrap(
            "aurix",
            "input_fresh",
            |c: Caller<'_, Host>, i: i32| -> i32 {
                c.data().inputs.get(i as usize).is_some_and(|x| x.3) as i32
            },
        )?;
        l.func_wrap(
            "aurix",
            "input_read",
            |mut c: Caller<'_, Host>, i: i32, ptr: i32, cap: i32| -> i32 {
                let Some(data) = c.data().inputs.get(i as usize).map(|x| x.2.clone()) else {
                    return 0;
                };
                let n = data.len().min(cap.max(0) as usize);
                if write_bytes(&mut c, ptr, &data[..n]) {
                    n as i32
                } else {
                    0
                }
            },
        )?;
        l.func_wrap(
            "aurix",
            "raise",
            |mut c: Caller<'_, Host>, code: i32, target: i32, ptr: i32, len: i32| {
                let Some(data) = read_bytes(&mut c, ptr, len.min(60_000)) else {
                    return;
                };
                if let Some(f) = frame(&mut c) {
                    f.raise(code as u16, actor(target), data);
                }
            },
        )?;
        l.func_wrap(
            "aurix",
            "hitbox",
            |mut c: Caller<'_, Host>,
             id: i64,
             cx: f32,
             cy: f32,
             cz: f32,
             hx: f32,
             hy: f32,
             hz: f32| {
                let h = &mut c.data_mut().hitboxes;
                if h.len() < 4096 {
                    h.push((
                        ent(id),
                        Hitbox {
                            center: [cx, cy, cz],
                            half: [hx.abs(), hy.abs(), hz.abs()],
                        },
                    ));
                }
            },
        )?;
        l.func_wrap(
            "aurix",
            "log",
            |mut c: Caller<'_, Host>, level: i32, ptr: i32, len: i32| {
                if c.data().log_budget == 0 {
                    return;
                }
                c.data_mut().log_budget -= 1;
                let Some(b) = read_bytes(&mut c, ptr, len.min(1024)) else {
                    return;
                };
                let msg = String::from_utf8_lossy(&b);
                match level {
                    0 => tracing::error!(target: "game", "{msg}"),
                    1 => tracing::warn!(target: "game", "{msg}"),
                    2 => tracing::info!(target: "game", "{msg}"),
                    _ => tracing::debug!(target: "game", "{msg}"),
                }
            },
        )?;
        Ok(())
    }

    /// Runs `f` with the frame exposed to host functions and a fresh fuel budget.
    fn call<R>(
        &mut self,
        frame: &mut Frame,
        f: impl FnOnce(&mut Store<Host>) -> Result<R>,
    ) -> Option<R> {
        if self.dead {
            return None;
        }
        self.store.data_mut().frame = frame as *mut Frame;
        self.store.data_mut().log_budget = LOG_BUDGET_PER_TICK;
        let _ = self.store.set_fuel(FUEL_PER_CALL);
        let r = f(&mut self.store);
        self.store.data_mut().frame = std::ptr::null_mut();
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.dead = true;
                tracing::error!(target: "game", error = %e, "game module trapped; logic disabled for this room");
                None
            }
        }
    }

    fn ensure_init(&mut self, frame: &mut Frame) {
        if self.initialized || self.dead {
            return;
        }
        self.initialized = true;
        let init = self.init.clone();
        let seed = self.seed as i64;
        self.call(frame, |s| init.call(s, seed));
    }

    pub fn on_join(&mut self, frame: &mut Frame, actor: ActorId) -> Option<EntityId> {
        self.ensure_init(frame);
        let join = self.join.clone()?;
        let id = self.call(frame, |s| join.call(s, actor.0 as i32))?;
        (id != 0).then(|| ent(id))
    }

    pub fn on_leave(&mut self, frame: &mut Frame, actor: ActorId, avatar: Option<EntityId>) {
        self.ensure_init(frame);
        let Some(leave) = self.leave.clone() else {
            if let Some(a) = avatar {
                let _ = frame.despawn(a);
            }
            return;
        };
        let e = avatar.map(handle).unwrap_or(0);
        self.call(frame, |s| leave.call(s, (actor.0 as i32, e)));
    }

    pub fn step(&mut self, frame: &mut Frame, inputs: &[StepInput]) {
        self.ensure_init(frame);
        self.store.data_mut().inputs = inputs.to_vec();
        self.store.data_mut().hitboxes.clear();
        let step = self.step.clone();
        self.call(frame, |s| step.call(s, ()));
        self.store.data_mut().inputs.clear();
    }

    /// Hitboxes the guest registered during the last `step`.
    pub fn hitboxes(&self) -> Vec<(EntityId, Hitbox)> {
        self.store.data().hitboxes.clone()
    }

    /// After a failover restore the guest starts from scratch (components carry the state).
    pub fn reset(&mut self) {
        self.initialized = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use net_sim::FrameConfig;
    use net_wire::schema::Schema;

    const SCHEMA: &str = r#"
component Transform {
    pos: vec3 @range(-4096, 4096, 0.01)
    speed: f32
}
component Score {
    points: u16
    name: string(16)
}
"#;

    // Each actor gets an avatar; input byte 0 = move x (i8). Step moves every avatar and
    // raises event 7 to its owner with the new x; registers a hitbox.
    const GUEST: &str = r#"
(module
  (import "aurix" "spawn" (func $spawn (param i32) (result i64)))
  (import "aurix" "despawn" (func $despawn (param i64) (result i32)))
  (import "aurix" "component_id" (func $cid (param i32 i32) (result i32)))
  (import "aurix" "get_vec3" (func $get_vec3 (param i64 i32 i32 i32) (result i32)))
  (import "aurix" "set_vec3" (func $set_vec3 (param i64 i32 i32 f32 f32 f32) (result i32)))
  (import "aurix" "set_f32" (func $set_f32 (param i64 i32 i32 f32) (result i32)))
  (import "aurix" "set_i64" (func $set_i64 (param i64 i32 i32 i64) (result i32)))
  (import "aurix" "set_bytes" (func $set_bytes (param i64 i32 i32 i32 i32) (result i32)))
  (import "aurix" "input_count" (func $input_count (result i32)))
  (import "aurix" "input_actor" (func $input_actor (param i32) (result i32)))
  (import "aurix" "input_entity" (func $input_entity (param i32) (result i64)))
  (import "aurix" "input_read" (func $input_read (param i32 i32 i32) (result i32)))
  (import "aurix" "raise" (func $raise (param i32 i32 i32 i32)))
  (import "aurix" "hitbox" (func $hitbox (param i64 f32 f32 f32 f32 f32 f32)))
  (import "aurix" "dt" (func $dt (result f32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "Transform")
  (data (i32.const 16) "Score")
  (data (i32.const 32) "bob")
  (global $tf (mut i32) (i32.const -1))
  (global $sc (mut i32) (i32.const -1))
  (func (export "aurix_init") (param $seed i64)
    (global.set $tf (call $cid (i32.const 0) (i32.const 9)))
    (global.set $sc (call $cid (i32.const 16) (i32.const 5))))
  (func (export "aurix_join") (param $actor i32) (result i64)
    (local $e i64)
    (local.set $e (call $spawn (local.get $actor)))
    (drop (call $set_vec3 (local.get $e) (global.get $tf) (i32.const 0) (f32.const 0) (f32.const 1) (f32.const 0)))
    (drop (call $set_f32 (local.get $e) (global.get $tf) (i32.const 1) (f32.const 6)))
    (drop (call $set_i64 (local.get $e) (global.get $sc) (i32.const 0) (i64.const 3)))
    (drop (call $set_bytes (local.get $e) (global.get $sc) (i32.const 1) (i32.const 32) (i32.const 3)))
    (local.get $e))
  (func (export "aurix_leave") (param $actor i32) (param $e i64)
    (drop (call $despawn (local.get $e))))
  (func (export "aurix_step")
    (local $i i32) (local $n i32) (local $e i64) (local $mx f32)
    (local.set $n (call $input_count))
    (block $done (loop $l
      (br_if $done (i32.ge_s (local.get $i) (local.get $n)))
      (local.set $e (call $input_entity (local.get $i)))
      (if (i64.ne (local.get $e) (i64.const 0)) (then
        ;; input byte 0 = i8 move_x -> write to scratch at 64
        (drop (call $input_read (local.get $i) (i32.const 64) (i32.const 4)))
        (local.set $mx (f32.convert_i32_s (i32.load8_s (i32.const 64))))
        (drop (call $get_vec3 (local.get $e) (global.get $tf) (i32.const 0) (i32.const 80)))
        (f32.store (i32.const 80)
          (f32.add (f32.load (i32.const 80))
            (f32.mul (f32.mul (local.get $mx) (f32.const 0.1)) (call $dt))))
        (drop (call $set_vec3 (local.get $e) (global.get $tf) (i32.const 0)
          (f32.load (i32.const 80)) (f32.load (i32.const 84)) (f32.load (i32.const 88))))
        (call $hitbox (local.get $e) (f32.load (i32.const 80)) (f32.const 1) (f32.const 0) (f32.const 0.5) (f32.const 1) (f32.const 0.5))
        (call $raise (i32.const 7) (call $input_actor (local.get $i)) (i32.const 80) (i32.const 4))))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br $l))))
)
"#;

    fn frame() -> Frame {
        Frame::new(
            Schema::parse(SCHEMA).unwrap(),
            FrameConfig {
                tick_hz: 60,
                ..Default::default()
            },
            1,
        )
    }

    #[test]
    fn guest_spawns_moves_and_raises() {
        let eng = GameEngine::new();
        let (hash, module) = eng.load(GUEST.as_bytes()).unwrap();
        assert_eq!(hash.len(), 64);
        let (h2, _) = eng.load(GUEST.as_bytes()).unwrap();
        assert_eq!(hash, h2);
        let mut logic = eng.instantiate(&module, 42).unwrap();
        let mut f = frame();
        let a = ActorId(1);
        let e = logic.on_join(&mut f, a).expect("avatar");
        assert_eq!(f.entity(e).unwrap().owner, Some(a));
        let sc = f.get(e, "Score").unwrap();
        assert_eq!(sc.fields[0], Value::UInt(3));
        assert_eq!(sc.fields[1], Value::String("bob".into()));
        assert_eq!(f.get(e, "Transform").unwrap().fields[1], Value::F32(6.0));

        // 60 ticks of +100 move_x: x += 100 * 0.1 * dt each tick -> ~10.0
        for _ in 0..60 {
            let inputs = vec![(a, Some(e), vec![100u8, 0, 0], true)];
            f.advance(|fr| logic.step(fr, &inputs));
        }
        let pos = match &f.get(e, "Transform").unwrap().fields[0] {
            Value::Vec3(v) => *v,
            _ => panic!(),
        };
        assert!((pos[0] - 10.0).abs() < 0.05, "x = {}", pos[0]);
        assert_eq!(pos[1], 1.0);
        let hb = logic.hitboxes();
        assert_eq!(hb.len(), 1);
        assert_eq!(hb[0].0, e);
        let ev = f.drain_events();
        assert_eq!(ev.len(), 60);
        assert_eq!((ev[0].code, ev[0].target), (7, Some(a)));
        assert!(!logic.dead);

        logic.on_leave(&mut f, a, Some(e));
        assert!(f.entity(e).is_none());
    }

    #[test]
    fn rejects_bad_modules_and_traps_are_contained() {
        let eng = GameEngine::new();
        assert!(matches!(eng.load(b"not wasm"), Err(GameError::Invalid(_))));
        let no_step =
            "(module (memory (export \"memory\") 1) (func (export \"aurix_init\") (param i64)))";
        assert!(matches!(
            eng.load(no_step.as_bytes()),
            Err(GameError::MissingExport("aurix_step"))
        ));
        // Infinite loop: fuel runs out, module is disabled, the frame survives.
        let spin = "(module (memory (export \"memory\") 1) (func (export \"aurix_init\") (param i64)) (func (export \"aurix_step\") (loop $l (br $l))))";
        let (_, m) = eng.load(spin.as_bytes()).unwrap();
        let mut logic = eng.instantiate(&m, 1).unwrap();
        let mut f = frame();
        let t = std::time::Instant::now();
        f.advance(|fr| logic.step(fr, &[]));
        assert!(logic.dead);
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
        f.advance(|fr| logic.step(fr, &[]));
        assert_eq!(f.tick.0, 2);
    }

    #[test]
    fn example_arena_module_moves_avatars() {
        let eng = GameEngine::new();
        let (_, module) = eng
            .load(include_str!("../examples/arena.wat").as_bytes())
            .unwrap();
        let mut logic = eng.instantiate(&module, 5).unwrap();
        let schema =
            "component Transform {\n    pos: vec3 @range(-4096, 4096, 0.01)\n    speed: f32\n}";
        let mut f = Frame::new(
            Schema::parse(schema).unwrap(),
            FrameConfig {
                tick_hz: 60,
                ..Default::default()
            },
            1,
        );
        let a = ActorId(3);
        let e = logic.on_join(&mut f, a).expect("avatar");
        let start = match &f.get(e, "Transform").unwrap().fields[0] {
            Value::Vec3(v) => *v,
            _ => panic!(),
        };
        assert_eq!(start, [-20.0, 0.0, -32.0]);
        for _ in 0..60 {
            let inputs = vec![(a, Some(e), vec![127u8, 0, 0], true)];
            f.advance(|fr| logic.step(fr, &inputs));
        }
        assert!(!logic.dead);
        let pos = match &f.get(e, "Transform").unwrap().fields[0] {
            Value::Vec3(v) => *v,
            _ => panic!(),
        };
        // speed 6 * 1s
        assert!((pos[0] - (start[0] + 6.0)).abs() < 0.05, "x = {}", pos[0]);
        assert_eq!(logic.hitboxes().len(), 1);
    }
}
