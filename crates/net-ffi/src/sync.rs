//! C ABI for sync rooms: a client-side replicated world (`net_sim::Frame` + the delta decoder the
//! node itself uses), acks, redundant input frames and owner-authoritative component writes:
//! the same shape as the Web `SyncWorld`/`SyncSession` and the Unity `SyncWorld`.
use crate::{cstr, set_client_error, set_error, AurixNetClient, ErrorCode};
use bytes::{BufMut, Bytes, BytesMut};
use net_client::margin::MarginControl;
use net_common::Channel;
use net_replication::{AckTracker, Replicator};
use net_sim::{EntityId, Frame, FrameConfig};
use net_wire::bits::BitWriter;
use net_wire::schema::{FieldType, Schema};
use net_wire::value::{ComponentValue, Value};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::ffi::c_char;

pub const KIND_INPUT: u8 = 0x10;
pub const KIND_DELTA: u8 = 0x11;
pub const KIND_ACK: u8 = 0x13;
pub const KIND_INPUT_STATS: u8 = 0x25;

pub const AURIX_NET_SYNC_NONE: i32 = 0;
pub const AURIX_NET_SYNC_SPAWN: i32 = 1;
pub const AURIX_NET_SYNC_CHANGE: i32 = 2;
pub const AURIX_NET_SYNC_DESPAWN: i32 = 3;

/// Redundant inputs per input frame (this tick + the previous ones), like the Web/Unity SDKs.
const INPUT_REDUNDANCY: usize = 3;

#[repr(C)]
pub struct AurixNetSyncEvent {
    pub kind: i32,
    pub entity: u64,
    /// Owning actor, -1 for server-owned entities.
    pub owner: i32,
}

struct State {
    frame: Frame,
    acks: AckTracker,
    server_tick: u32,
    last_input_tick: u32,
    margin: MarginControl,
    /// Ticks the margin reports have added to the lead (negative: taken off it).
    lead_adjust: i32,
    recent: VecDeque<Vec<u8>>,
    /// Inputs waiting for a tick: (body, keep). Writes and raw inputs are replaced by newer ones.
    held: VecDeque<(Vec<u8>, bool)>,
    events: VecDeque<AurixNetSyncEvent>,
    /// Component write under construction (`write_begin` .. `write_end`).
    current: Option<(u16, ComponentValue)>,
    writes: Vec<(u16, ComponentValue)>,
}

pub struct AurixNetSync {
    inner: Mutex<State>,
}

fn owner_of(frame: &Frame, id: EntityId) -> i32 {
    frame
        .entity(id)
        .and_then(|e| e.owner)
        .map_or(-1, |a| i32::from(a.0))
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_new(schema: *const c_char) -> *mut AurixNetSync {
    let Some(text) = cstr(schema) else {
        set_error(ErrorCode::InvalidArgument, "schema: null or non-utf8");
        return std::ptr::null_mut();
    };
    let schema = match Schema::parse(text) {
        Ok(s) => s,
        Err(e) => {
            set_error(ErrorCode::InvalidArgument, format!("schema: {e}"));
            return std::ptr::null_mut();
        }
    };
    Box::into_raw(Box::new(AurixNetSync {
        inner: Mutex::new(State {
            frame: Frame::new(schema, FrameConfig::default(), 1),
            acks: AckTracker::default(),
            server_tick: 0,
            last_input_tick: 0,
            margin: MarginControl::default(),
            lead_adjust: 0,
            recent: VecDeque::new(),
            held: VecDeque::new(),
            events: VecDeque::new(),
            current: None,
            writes: Vec::new(),
        }),
    }))
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_destroy(s: *mut AurixNetSync) {
    if !s.is_null() {
        drop(Box::from_raw(s));
    }
}

/// Feeds one data-path payload. 1 = delta applied and `ack` (9 bytes, may be NULL) filled, send it
/// on the unreliable channel; 2 = input margin report taken, the lead of the next inputs follows
/// it; 0 = not a sync frame; -1 = error (`aurix_net_last_error`).
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_receive(
    s: *const AurixNetSync,
    data: *const u8,
    len: usize,
    ack: *mut u8,
) -> i32 {
    let Some(s) = s.as_ref() else { return -1 };
    if data.is_null() || len < 7 {
        return 0;
    }
    let p = std::slice::from_raw_parts(data, len);
    let mut st = s.inner.lock();
    if p[0] == KIND_INPUT_STATS {
        let tick = u32::from_be_bytes([p[1], p[2], p[3], p[4]]);
        let stamped = st.last_input_tick;
        st.lead_adjust += st.margin.adjust(tick, p[5] as i8, stamped);
        return 2;
    }
    if p[0] != KIND_DELTA || len < 9 {
        return 0;
    }
    let report = match Replicator::apply_report(&mut st.frame, &p[1..]) {
        Ok(r) => r,
        Err(e) => {
            set_error(ErrorCode::Protocol, format!("delta: {e}"));
            return -1;
        }
    };
    st.server_tick = report.tick.0;
    st.acks.on_delta(report.tick.0, report.baseline);
    for id in &report.despawned {
        st.events.push_back(AurixNetSyncEvent {
            kind: AURIX_NET_SYNC_DESPAWN,
            entity: id.to_u64(),
            owner: -1,
        });
    }
    for (kind, ids) in [
        (AURIX_NET_SYNC_SPAWN, &report.spawned),
        (AURIX_NET_SYNC_CHANGE, &report.changed),
    ] {
        for id in ids {
            let owner = owner_of(&st.frame, *id);
            st.events.push_back(AurixNetSyncEvent {
                kind,
                entity: id.to_u64(),
                owner,
            });
        }
    }
    if !ack.is_null() {
        if let Some((last, bits)) = st.acks.ack() {
            let out = std::slice::from_raw_parts_mut(ack, 9);
            out[0] = KIND_ACK;
            out[1..5].copy_from_slice(&last.to_be_bytes());
            out[5..9].copy_from_slice(&bits.to_be_bytes());
        }
    }
    1
}

/// Pops the next spawn/change/despawn; 1 = `out` filled, 0 = none.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_poll(
    s: *const AurixNetSync,
    out: *mut AurixNetSyncEvent,
) -> i32 {
    let (Some(s), Some(out)) = (s.as_ref(), out.as_mut()) else {
        return -1;
    };
    match s.inner.lock().events.pop_front() {
        Some(ev) => {
            *out = ev;
            1
        }
        None => {
            out.kind = AURIX_NET_SYNC_NONE;
            0
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_server_tick(s: *const AurixNetSync) -> u32 {
    s.as_ref().map_or(0, |s| s.inner.lock().server_tick)
}

/// Deltas the node sent that never arrived (unrepaired until the node resends in full).
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_gaps(s: *const AurixNetSync) -> u64 {
    s.as_ref().map_or(0, |s| s.inner.lock().acks.gaps)
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_entity_count(s: *const AurixNetSync) -> i32 {
    s.as_ref()
        .map_or(-1, |s| s.inner.lock().frame.entity_count() as i32)
}

/// Entity `index` (0..count) in id order; 1 = filled, 0 = out of range.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_entity_at(
    s: *const AurixNetSync,
    index: i32,
    id: *mut u64,
    owner: *mut i32,
) -> i32 {
    let Some(s) = s.as_ref() else { return -1 };
    if index < 0 {
        return 0;
    }
    let st = s.inner.lock();
    let Some(e) = st.frame.entities().nth(index as usize) else {
        return 0;
    };
    if let Some(id) = id.as_mut() {
        *id = e.id.to_u64();
    }
    if let Some(o) = owner.as_mut() {
        *o = e.owner.map_or(-1, |a| i32::from(a.0));
    }
    1
}

/// Component id by schema name, -1 if unknown.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_component(
    s: *const AurixNetSync,
    name: *const c_char,
) -> i32 {
    let (Some(s), Some(name)) = (s.as_ref(), cstr(name)) else {
        return -1;
    };
    s.inner
        .lock()
        .frame
        .schema
        .component(name)
        .map_or(-1, |c| i32::from(c.id))
}

/// Field index inside a component by name, -1 if unknown.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_field(
    s: *const AurixNetSync,
    component: i32,
    name: *const c_char,
) -> i32 {
    let (Some(s), Some(name)) = (s.as_ref(), cstr(name)) else {
        return -1;
    };
    let st = s.inner.lock();
    let Some(c) = u16::try_from(component)
        .ok()
        .and_then(|id| st.frame.schema.by_id(id))
    else {
        return -1;
    };
    c.fields
        .iter()
        .position(|f| f.name == name)
        .map_or(-1, |i| i as i32)
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_has(
    s: *const AurixNetSync,
    entity: u64,
    component: i32,
) -> i32 {
    let Some(s) = s.as_ref() else { return -1 };
    let Ok(cid) = u16::try_from(component) else {
        return 0;
    };
    s.inner
        .lock()
        .frame
        .entity(EntityId::from_u64(entity))
        .is_some_and(|e| e.components.contains_key(&cid)) as i32
}

fn field_value<R>(
    st: &State,
    entity: u64,
    component: i32,
    field: i32,
    f: impl FnOnce(&Value) -> Option<R>,
) -> Option<R> {
    let cid = u16::try_from(component).ok()?;
    let v = st
        .frame
        .entity(EntityId::from_u64(entity))?
        .components
        .get(&cid)?
        .fields
        .get(usize::try_from(field).ok()?)?;
    f(v)
}

/// Scalar field (bool, int, uint, f32, quantized) as a double; 1 = filled, 0 = missing or not a scalar.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_get_number(
    s: *const AurixNetSync,
    entity: u64,
    component: i32,
    field: i32,
    out: *mut f64,
) -> i32 {
    let (Some(s), Some(out)) = (s.as_ref(), out.as_mut()) else {
        return -1;
    };
    let st = s.inner.lock();
    match field_value(&st, entity, component, field, |v| match v {
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::UInt(u) => Some(*u as f64),
        Value::Int(i) => Some(*i as f64),
        Value::F32(f) => Some(f64::from(*f)),
        _ => None,
    }) {
        Some(v) => {
            *out = v;
            1
        }
        None => 0,
    }
}

/// Vector field into `out[4]`: returns the number of components written (2 vec2, 3 vec3, 4 quat x,y,z,w;
/// 1 for scalars), 0 = missing.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_get_vector(
    s: *const AurixNetSync,
    entity: u64,
    component: i32,
    field: i32,
    out: *mut f32,
) -> i32 {
    let Some(s) = s.as_ref() else { return -1 };
    if out.is_null() {
        return -1;
    }
    let st = s.inner.lock();
    let o = std::slice::from_raw_parts_mut(out, 4);
    field_value(&st, entity, component, field, |v| match v {
        Value::Vec2(a) => {
            o[..2].copy_from_slice(a);
            Some(2)
        }
        Value::Vec3(a) => {
            o[..3].copy_from_slice(a);
            Some(3)
        }
        Value::Quat(a) => {
            o.copy_from_slice(a);
            Some(4)
        }
        Value::F32(f) => {
            o[0] = *f;
            Some(1)
        }
        Value::Int(i) => {
            o[0] = *i as f32;
            Some(1)
        }
        Value::UInt(u) => {
            o[0] = *u as f32;
            Some(1)
        }
        Value::Bool(b) => {
            o[0] = f32::from(u8::from(*b));
            Some(1)
        }
        _ => None,
    })
    .unwrap_or(0)
}

/// String/bytes field: copies up to `cap` bytes into `buf` (may be NULL to just measure) and
/// returns the full length; -1 = missing or not string/bytes.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_get_bytes(
    s: *const AurixNetSync,
    entity: u64,
    component: i32,
    field: i32,
    buf: *mut u8,
    cap: usize,
) -> i32 {
    let Some(s) = s.as_ref() else { return -1 };
    let st = s.inner.lock();
    field_value(&st, entity, component, field, |v| {
        let bytes: &[u8] = match v {
            Value::String(t) => t.as_bytes(),
            Value::Bytes(b) => b,
            _ => return None,
        };
        if !buf.is_null() && cap > 0 {
            let n = bytes.len().min(cap);
            std::slice::from_raw_parts_mut(buf, n).copy_from_slice(&bytes[..n]);
        }
        Some(bytes.len() as i32)
    })
    .unwrap_or(-1)
}

fn encode_input(first_tick: u32, inputs: &VecDeque<Vec<u8>>) -> Bytes {
    let mut b =
        BytesMut::with_capacity(6 + inputs.iter().map(|i| 1 + i.len().min(255)).sum::<usize>());
    b.put_u8(KIND_INPUT);
    b.put_u32(first_tick);
    b.put_u8(inputs.len().min(255) as u8);
    for i in inputs.iter().take(255) {
        let n = i.len().min(255);
        b.put_u8(n as u8);
        b.put_slice(&i[..n]);
    }
    b.freeze()
}

const DEFAULT_TICK_HZ: u16 = 60;
/// The room drops inputs more than 64 ticks ahead of it.
const MAX_LEAD_TICKS: u32 = 60;

/// The room takes one input per tick, drops an input whose tick it has already simulated and
/// applies one stamped far ahead that much later. So an input is stamped ahead of the last delta
/// by the round trip plus two ticks, and no further; the rest waits in `held` for the next delta.
unsafe fn send_input_inner(
    st: &mut State,
    h: *mut AurixNetClient,
    body: Vec<u8>,
    keep: bool,
) -> i64 {
    match st.held.back_mut() {
        Some(tail) if !tail.1 && !keep => tail.0 = body,
        _ => st.held.push_back((body, keep)),
    }
    if pump(st, h) < 0 {
        return -1;
    }
    i64::from(st.last_input_tick) + st.held.len() as i64
}

unsafe fn pump(st: &mut State, h: *mut AurixNetClient) -> i32 {
    let Some(h) = h.as_ref() else { return -1 };
    let estimate = {
        let client = h.inner.lock();
        let tick_hz = client.joined.as_ref().and_then(|joined| joined.tick_hz);
        let round_trip = client.rtt_ms() * u32::from(tick_hz.unwrap_or(DEFAULT_TICK_HZ));
        2 + round_trip.div_ceil(1000).min(MAX_LEAD_TICKS) as i32
    };
    // The round trip gives the first guess; the room's margin reports correct it.
    let lead = (estimate + st.lead_adjust).clamp(1, MAX_LEAD_TICKS as i32 + 2);
    st.lead_adjust = lead - estimate;
    let lead = lead as u32;
    while !st.held.is_empty() {
        let tick = (st.server_tick + lead).max(st.last_input_tick + 1);
        if tick > st.server_tick + lead + 1 {
            break;
        }
        let Some((body, _)) = st.held.pop_front() else {
            break;
        };
        // a frame carries inputs for consecutive ticks, so earlier ones are resent only without a gap
        if tick != st.last_input_tick + 1 {
            st.recent.clear();
        }
        st.last_input_tick = tick;
        st.recent.push_back(body);
        while st.recent.len() > INPUT_REDUNDANCY {
            st.recent.pop_front();
        }
        let first = tick - (st.recent.len() as u32 - 1);
        let frame = encode_input(first, &st.recent);
        let Some(ch) = Channel::from_u8(0) else {
            return -1;
        };
        if let Err(e) = h.inner.lock().send_data(ch, frame) {
            set_client_error(&e);
            return -1;
        }
    }
    0
}

/// Sends the inputs that were waiting for a tick. Call after every delta
/// (`aurix_net_sync_receive` returned 1). 0 on success, -1 on error.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_pump(
    s: *const AurixNetSync,
    h: *mut AurixNetClient,
) -> i32 {
    let Some(s) = s.as_ref() else { return -1 };
    pump(&mut s.inner.lock(), h)
}

/// Raw input for server-side logic (rooms with `props.logic`): stamped ahead of the last delta by the
/// round trip plus two ticks and resent with the next two inputs. Returns the stamped tick or -1.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_send_input(
    s: *const AurixNetSync,
    h: *mut AurixNetClient,
    data: *const u8,
    len: usize,
) -> i64 {
    let Some(s) = s.as_ref() else { return -1 };
    let body = if data.is_null() || len == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(data, len).to_vec()
    };
    let mut st = s.inner.lock();
    send_input_inner(&mut st, h, body, false)
}

/// Starts a full write of `component` (fields default to zero until written).
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_write_begin(s: *const AurixNetSync, component: i32) -> i32 {
    let Some(s) = s.as_ref() else { return -1 };
    let mut st = s.inner.lock();
    let Some(c) = u16::try_from(component)
        .ok()
        .and_then(|id| st.frame.schema.by_id(id))
    else {
        set_error(ErrorCode::InvalidArgument, "write_begin: unknown component");
        return -1;
    };
    let v = ComponentValue::zero(c);
    st.current = Some((c.id, v));
    0
}

fn with_field(st: &mut State, field: i32, f: impl FnOnce(&FieldType) -> Option<Value>) -> i32 {
    let Some((cid, cur)) = st.current.as_mut() else {
        set_error(ErrorCode::InvalidArgument, "write: call write_begin first");
        return -1;
    };
    let Some(ty) = st
        .frame
        .schema
        .by_id(*cid)
        .and_then(|c| c.fields.get(usize::try_from(field).ok()?))
        .map(|fd| fd.ty.clone())
    else {
        set_error(ErrorCode::InvalidArgument, "write: unknown field");
        return -1;
    };
    match f(&ty) {
        Some(v) => {
            cur.fields[field as usize] = v;
            0
        }
        None => {
            set_error(
                ErrorCode::InvalidArgument,
                "write: value does not fit the field type",
            );
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_write_number(
    s: *const AurixNetSync,
    field: i32,
    v: f64,
) -> i32 {
    let Some(s) = s.as_ref() else { return -1 };
    let mut st = s.inner.lock();
    with_field(&mut st, field, |ty| match ty {
        FieldType::Bool => Some(Value::Bool(v != 0.0)),
        FieldType::UInt { .. } => Some(Value::UInt(v.max(0.0) as u64)),
        FieldType::Int { .. } => Some(Value::Int(v as i64)),
        FieldType::F32 | FieldType::Quant { .. } => Some(Value::F32(v as f32)),
        _ => None,
    })
}

/// `n` = 2 (vec2), 3 (vec3) or 4 (quat x,y,z,w).
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_write_vector(
    s: *const AurixNetSync,
    field: i32,
    v: *const f32,
    n: i32,
) -> i32 {
    let Some(s) = s.as_ref() else { return -1 };
    if v.is_null() || !(1..=4).contains(&n) {
        set_error(
            ErrorCode::InvalidArgument,
            "write_vector: bad pointer or count",
        );
        return -1;
    }
    let a = std::slice::from_raw_parts(v, n as usize);
    let mut st = s.inner.lock();
    with_field(&mut st, field, |ty| match (ty, a.len()) {
        (FieldType::Vec2 { .. }, 2) => Some(Value::Vec2([a[0], a[1]])),
        (FieldType::Vec3 { .. }, 3) => Some(Value::Vec3([a[0], a[1], a[2]])),
        (FieldType::Quat, 4) => Some(Value::Quat([a[0], a[1], a[2], a[3]])),
        (FieldType::F32 | FieldType::Quant { .. }, 1) => Some(Value::F32(a[0])),
        _ => None,
    })
}

/// String (UTF-8, invalid bytes replaced) or bytes field.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_write_bytes(
    s: *const AurixNetSync,
    field: i32,
    p: *const u8,
    n: usize,
) -> i32 {
    let Some(s) = s.as_ref() else { return -1 };
    let bytes = if p.is_null() || n == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(p, n).to_vec()
    };
    let mut st = s.inner.lock();
    with_field(&mut st, field, |ty| match ty {
        FieldType::String { max } => {
            let mut t = String::from_utf8_lossy(&bytes).into_owned();
            while t.len() > *max as usize {
                t.pop();
            }
            Some(Value::String(t))
        }
        FieldType::Bytes { max } => {
            let mut b = bytes.clone();
            b.truncate(*max as usize);
            Some(Value::Bytes(b))
        }
        _ => None,
    })
}

/// Finishes the current component write (queued until `send_writes`).
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_write_end(s: *const AurixNetSync) -> i32 {
    let Some(s) = s.as_ref() else { return -1 };
    let mut st = s.inner.lock();
    match st.current.take() {
        Some(w) => {
            st.writes.push(w);
            0
        }
        None => {
            set_error(ErrorCode::InvalidArgument, "write_end: nothing begun");
            -1
        }
    }
}

/// Queued writes as `u8 n, n × (cid, encode_full)`; -1 (with error) on a bad write.
fn encode_queued_writes(st: &mut State, w: &mut BitWriter) -> i32 {
    let writes = std::mem::take(&mut st.writes);
    let cid_bits = st.frame.schema.component_id_bits();
    w.write_bits(writes.len().min(255) as u64, 8);
    for (cid, v) in writes.iter().take(255) {
        let Some(c) = st.frame.schema.by_id(*cid) else {
            return -1;
        };
        w.write_bits(u64::from(*cid), cid_bits);
        if let Err(e) = v.encode_full(c, w) {
            set_error(ErrorCode::InvalidArgument, format!("writes: {e}"));
            return -1;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_send_writes(
    s: *const AurixNetSync,
    h: *mut AurixNetClient,
) -> i64 {
    let Some(s) = s.as_ref() else { return -1 };
    let mut st = s.inner.lock();
    let mut w = BitWriter::new();
    if encode_queued_writes(&mut st, &mut w) != 0 {
        return -1;
    }
    let body = w.finish();
    send_input_inner(&mut st, h, body, false)
}

/// One owner-authoritative op as this tick's input (rooms with a schema and no `logic`):
/// `op` 0 = write `entity` with the queued writes, 1 = spawn a new entity you own (queued
/// writes are its initial state; at most 32 per actor), 2 = despawn `entity`, 3 = hand `entity`
/// to actor `to`, 4 = set the ownership policy of `entity` to `to` (0 destroy_on_leave, 1 to_master,
/// 2 orphan, 3 dynamic), 5 = request ownership of `entity` (unowned, or policy dynamic), 6 = release
/// `entity` (stays alive, unowned). Only entities you own except 5; the node ignores anything else.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_send_op(
    s: *const AurixNetSync,
    h: *mut AurixNetClient,
    op: i32,
    entity: u64,
    to: i32,
) -> i64 {
    let Some(s) = s.as_ref() else { return -1 };
    if !(0..=6).contains(&op) {
        set_error(ErrorCode::InvalidArgument, "send_op: op must be 0..=6");
        return -1;
    }
    let mut st = s.inner.lock();
    let mut w = BitWriter::new();
    w.write_bits(0xFF, 8);
    w.write_bits(1, 8);
    w.write_bits(op as u64, 8);
    if op != 1 {
        w.write_bits(entity, 48);
    }
    match op {
        0 | 1 => {
            if encode_queued_writes(&mut st, &mut w) != 0 {
                return -1;
            }
        }
        3 => w.write_bits(to.clamp(0, 0xFFFF) as u64, 16),
        4 => w.write_bits(to.clamp(0, 3) as u64, 8),
        _ => {}
    }
    let body = w.finish();
    send_input_inner(&mut st, h, body, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use net_common::ActorId;
    use net_replication::{ClientView, ReplicationConfig};
    use std::ffi::CString;

    const SCHEMA: &str = "component Transform {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n}\ncomponent Tag {\n  name: string(16)\n  hp: u8\n}\n";

    #[test]
    fn delta_in_fields_out_ack_and_writes() {
        let schema = Schema::parse(SCHEMA).unwrap();
        let mut server = Frame::new(schema.clone(), FrameConfig::default(), 7);
        let id = server.spawn(Some(ActorId(3))).unwrap();
        server
            .set(
                id,
                "Transform",
                ComponentValue {
                    fields: vec![
                        Value::Vec3([1.5, -2.0, 3.25]),
                        Value::Quat([0.0, 0.0, 0.0, 1.0]),
                    ],
                },
            )
            .unwrap();
        server
            .set(
                id,
                "Tag",
                ComponentValue {
                    fields: vec![Value::String("bob".into()), Value::UInt(90)],
                },
            )
            .unwrap();
        for _ in 0..12 {
            server.advance(|_| {});
        }
        let rep = Replicator::new(ReplicationConfig::default(), &schema);
        let mut view = ClientView::new(ActorId(1));
        let mut delta = vec![KIND_DELTA];
        delta.extend(rep.build(&server, &mut view));

        let cschema = CString::new(SCHEMA).unwrap();
        unsafe {
            let s = aurix_net_sync_new(cschema.as_ptr());
            assert!(!s.is_null());
            let mut ack = [0u8; 9];
            assert_eq!(
                aurix_net_sync_receive(s, delta.as_ptr(), delta.len(), ack.as_mut_ptr()),
                1
            );
            assert_eq!(ack[0], KIND_ACK);
            assert_eq!(u32::from_be_bytes(ack[1..5].try_into().unwrap()), 12);
            assert_eq!(aurix_net_sync_server_tick(s), 12);
            let mut ev = AurixNetSyncEvent {
                kind: 0,
                entity: 0,
                owner: 0,
            };
            assert_eq!(aurix_net_sync_poll(s, &mut ev), 1);
            assert_eq!(
                (ev.kind, ev.entity, ev.owner),
                (AURIX_NET_SYNC_SPAWN, id.to_u64(), 3)
            );
            assert_eq!(aurix_net_sync_poll(s, &mut ev), 0);
            let tr = aurix_net_sync_component(s, c"Transform".as_ptr());
            let tag = aurix_net_sync_component(s, c"Tag".as_ptr());
            assert!(tr >= 0 && tag >= 0);
            assert_eq!(aurix_net_sync_component(s, c"Nope".as_ptr()), -1);
            let pos = aurix_net_sync_field(s, tr, c"pos".as_ptr());
            let hp = aurix_net_sync_field(s, tag, c"hp".as_ptr());
            let name = aurix_net_sync_field(s, tag, c"name".as_ptr());
            let mut v = [0f32; 4];
            assert_eq!(
                aurix_net_sync_get_vector(s, id.to_u64(), tr, pos, v.as_mut_ptr()),
                3
            );
            assert!(
                (v[0] - 1.5).abs() < 0.01
                    && (v[1] + 2.0).abs() < 0.01
                    && (v[2] - 3.25).abs() < 0.01
            );
            let mut d = 0f64;
            assert_eq!(
                aurix_net_sync_get_number(s, id.to_u64(), tag, hp, &mut d),
                1
            );
            assert_eq!(d, 90.0);
            let mut buf = [0u8; 16];
            assert_eq!(
                aurix_net_sync_get_bytes(s, id.to_u64(), tag, name, buf.as_mut_ptr(), 16),
                3
            );
            assert_eq!(&buf[..3], b"bob");
            assert_eq!(aurix_net_sync_entity_count(s), 1);
            assert_eq!(aurix_net_sync_has(s, id.to_u64(), tag), 1);

            // a write round-trips through the schema codec
            assert_eq!(aurix_net_sync_write_begin(s, tr), 0);
            let p = [4.0f32, 5.0, 6.0];
            assert_eq!(aurix_net_sync_write_vector(s, pos, p.as_ptr(), 3), 0);
            assert_eq!(aurix_net_sync_write_number(s, pos, 1.0), -1);
            assert_eq!(aurix_net_sync_write_end(s), 0);
            {
                let st = s.as_ref().unwrap().inner.lock();
                assert_eq!(st.writes.len(), 1);
                assert_eq!(st.writes[0].1.fields[0], Value::Vec3([4.0, 5.0, 6.0]));
            }
            // not a delta
            let junk = [0x20u8, 1, 2, 3, 4, 5, 6, 7, 8, 9];
            assert_eq!(
                aurix_net_sync_receive(s, junk.as_ptr(), junk.len(), std::ptr::null_mut()),
                0
            );
            aurix_net_sync_destroy(s);
        }
    }
}
