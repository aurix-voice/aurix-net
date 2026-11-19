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
        .and_then(|entity| entity.owner)
        .map_or(-1, |actor| i32::from(actor.0))
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_new(schema: *const c_char) -> *mut AurixNetSync {
    let Some(text) = cstr(schema) else {
        set_error(ErrorCode::InvalidArgument, "schema: null or non-utf8");
        return std::ptr::null_mut();
    };
    let schema = match Schema::parse(text) {
        Ok(schema) => schema,
        Err(error) => {
            set_error(ErrorCode::InvalidArgument, format!("schema: {error}"));
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
pub unsafe extern "C" fn aurix_net_sync_destroy(sync: *mut AurixNetSync) {
    if !sync.is_null() {
        drop(Box::from_raw(sync));
    }
}

/// Feeds one data-path payload. 1 = delta applied and `ack` (9 bytes, may be NULL) filled, send it
/// on the unreliable channel; 2 = input margin report taken, the lead of the next inputs follows
/// it; 0 = not a sync frame; -1 = error (`aurix_net_last_error`).
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_receive(
    sync: *const AurixNetSync,
    data: *const u8,
    len: usize,
    ack: *mut u8,
) -> i32 {
    let Some(sync) = sync.as_ref() else { return -1 };
    if data.is_null() || len < 7 {
        return 0;
    }
    let frame = std::slice::from_raw_parts(data, len);
    let mut st = sync.inner.lock();
    if frame[0] == KIND_INPUT_STATS {
        let tick = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]);
        let stamped = st.last_input_tick;
        st.lead_adjust += st.margin.adjust(tick, frame[5] as i8, stamped);
        return 2;
    }
    if frame[0] != KIND_DELTA || len < 9 {
        return 0;
    }
    let report = match Replicator::apply_report(&mut st.frame, &frame[1..]) {
        Ok(report) => report,
        Err(error) => {
            set_error(ErrorCode::Protocol, format!("delta: {error}"));
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
    sync: *const AurixNetSync,
    out: *mut AurixNetSyncEvent,
) -> i32 {
    let (Some(sync), Some(out)) = (sync.as_ref(), out.as_mut()) else {
        return -1;
    };
    match sync.inner.lock().events.pop_front() {
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
pub unsafe extern "C" fn aurix_net_sync_server_tick(sync: *const AurixNetSync) -> u32 {
    sync.as_ref()
        .map_or(0, |sync| sync.inner.lock().server_tick)
}

/// Deltas the node sent that never arrived (unrepaired until the node resends in full).
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_gaps(sync: *const AurixNetSync) -> u64 {
    sync.as_ref().map_or(0, |sync| sync.inner.lock().acks.gaps)
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_entity_count(sync: *const AurixNetSync) -> i32 {
    sync.as_ref()
        .map_or(-1, |sync| sync.inner.lock().frame.entity_count() as i32)
}

/// Entity `index` (0..count) in id order; 1 = filled, 0 = out of range.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_entity_at(
    sync: *const AurixNetSync,
    index: i32,
    id: *mut u64,
    owner: *mut i32,
) -> i32 {
    let Some(sync) = sync.as_ref() else { return -1 };
    if index < 0 {
        return 0;
    }
    let st = sync.inner.lock();
    let Some(found) = st.frame.entities().nth(index as usize) else {
        return 0;
    };
    if let Some(id) = id.as_mut() {
        *id = found.id.to_u64();
    }
    if let Some(owner) = owner.as_mut() {
        *owner = found.owner.map_or(-1, |actor| i32::from(actor.0));
    }
    1
}

/// Component id by schema name, -1 if unknown.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_component(
    sync: *const AurixNetSync,
    name: *const c_char,
) -> i32 {
    let (Some(sync), Some(name)) = (sync.as_ref(), cstr(name)) else {
        return -1;
    };
    sync.inner
        .lock()
        .frame
        .schema
        .component(name)
        .map_or(-1, |found| i32::from(found.id))
}

/// Field index inside a component by name, -1 if unknown.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_field(
    sync: *const AurixNetSync,
    component: i32,
    name: *const c_char,
) -> i32 {
    let (Some(sync), Some(name)) = (sync.as_ref(), cstr(name)) else {
        return -1;
    };
    let st = sync.inner.lock();
    let Some(found) = u16::try_from(component)
        .ok()
        .and_then(|id| st.frame.schema.by_id(id))
    else {
        return -1;
    };
    found
        .fields
        .iter()
        .position(|field| field.name == name)
        .map_or(-1, |index| index as i32)
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_has(
    sync: *const AurixNetSync,
    entity: u64,
    component: i32,
) -> i32 {
    let Some(sync) = sync.as_ref() else { return -1 };
    let Ok(cid) = u16::try_from(component) else {
        return 0;
    };
    sync.inner
        .lock()
        .frame
        .entity(EntityId::from_u64(entity))
        .is_some_and(|found| found.components.contains_key(&cid)) as i32
}

fn field_value<R>(
    st: &State,
    entity: u64,
    component: i32,
    field: i32,
    read: impl FnOnce(&Value) -> Option<R>,
) -> Option<R> {
    let cid = u16::try_from(component).ok()?;
    let value = st
        .frame
        .entity(EntityId::from_u64(entity))?
        .components
        .get(&cid)?
        .fields
        .get(usize::try_from(field).ok()?)?;
    read(value)
}

/// Scalar field (bool, int, uint, f32, quantized) as a double; 1 = filled, 0 = missing or not a scalar.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_get_number(
    sync: *const AurixNetSync,
    entity: u64,
    component: i32,
    field: i32,
    out: *mut f64,
) -> i32 {
    let (Some(sync), Some(out)) = (sync.as_ref(), out.as_mut()) else {
        return -1;
    };
    let st = sync.inner.lock();
    match field_value(&st, entity, component, field, |value| match value {
        Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
        Value::UInt(number) => Some(*number as f64),
        Value::Int(number) => Some(*number as f64),
        Value::F32(number) => Some(f64::from(*number)),
        _ => None,
    }) {
        Some(number) => {
            *out = number;
            1
        }
        None => 0,
    }
}

/// Vector field into `out[4]`: returns the number of components written (2 vec2, 3 vec3, 4 quat x,y,z,w;
/// 1 for scalars), 0 = missing.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_get_vector(
    sync: *const AurixNetSync,
    entity: u64,
    component: i32,
    field: i32,
    out: *mut f32,
) -> i32 {
    let Some(sync) = sync.as_ref() else { return -1 };
    if out.is_null() {
        return -1;
    }
    let st = sync.inner.lock();
    let out = std::slice::from_raw_parts_mut(out, 4);
    field_value(&st, entity, component, field, |value| match value {
        Value::Vec2(vector) => {
            out[..2].copy_from_slice(vector);
            Some(2)
        }
        Value::Vec3(vector) => {
            out[..3].copy_from_slice(vector);
            Some(3)
        }
        Value::Quat(quat) => {
            out.copy_from_slice(quat);
            Some(4)
        }
        Value::F32(number) => {
            out[0] = *number;
            Some(1)
        }
        Value::Int(number) => {
            out[0] = *number as f32;
            Some(1)
        }
        Value::UInt(number) => {
            out[0] = *number as f32;
            Some(1)
        }
        Value::Bool(flag) => {
            out[0] = f32::from(u8::from(*flag));
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
    sync: *const AurixNetSync,
    entity: u64,
    component: i32,
    field: i32,
    buf: *mut u8,
    cap: usize,
) -> i32 {
    let Some(sync) = sync.as_ref() else { return -1 };
    let st = sync.inner.lock();
    field_value(&st, entity, component, field, |value| {
        let bytes: &[u8] = match value {
            Value::String(text) => text.as_bytes(),
            Value::Bytes(bytes) => bytes,
            _ => return None,
        };
        if !buf.is_null() && cap > 0 {
            let copied = bytes.len().min(cap);
            std::slice::from_raw_parts_mut(buf, copied).copy_from_slice(&bytes[..copied]);
        }
        Some(bytes.len() as i32)
    })
    .unwrap_or(-1)
}

fn encode_input(first_tick: u32, inputs: &VecDeque<Vec<u8>>) -> Bytes {
    let size = 6 + inputs
        .iter()
        .map(|input| 1 + input.len().min(255))
        .sum::<usize>();
    let mut frame = BytesMut::with_capacity(size);
    frame.put_u8(KIND_INPUT);
    frame.put_u32(first_tick);
    frame.put_u8(inputs.len().min(255) as u8);
    for input in inputs.iter().take(255) {
        let len = input.len().min(255);
        frame.put_u8(len as u8);
        frame.put_slice(&input[..len]);
    }
    frame.freeze()
}

const DEFAULT_TICK_HZ: u16 = 60;
/// The room drops inputs more than 64 ticks ahead of it.
const MAX_LEAD_TICKS: u32 = 60;

/// The room takes one input per tick, drops an input whose tick it has already simulated and
/// applies one stamped far ahead that much later. So an input is stamped ahead of the last delta
/// by the round trip plus two ticks, and no further; the rest waits in `held` for the next delta.
unsafe fn send_input_inner(
    st: &mut State,
    client: *mut AurixNetClient,
    body: Vec<u8>,
    keep: bool,
) -> i64 {
    match st.held.back_mut() {
        Some(tail) if !tail.1 && !keep => tail.0 = body,
        _ => st.held.push_back((body, keep)),
    }
    if pump(st, client) < 0 {
        return -1;
    }
    i64::from(st.last_input_tick) + st.held.len() as i64
}

unsafe fn pump(st: &mut State, client: *mut AurixNetClient) -> i32 {
    let Some(client) = client.as_ref() else {
        return -1;
    };
    let estimate = {
        let inner = client.inner.lock();
        let tick_hz = inner.joined.as_ref().and_then(|joined| joined.tick_hz);
        let round_trip = inner.rtt_ms() * u32::from(tick_hz.unwrap_or(DEFAULT_TICK_HZ));
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
        let Some(channel) = Channel::from_u8(0) else {
            return -1;
        };
        if let Err(error) = client.inner.lock().send_data(channel, frame) {
            set_client_error(&error);
            return -1;
        }
    }
    0
}

/// Sends the inputs that were waiting for a tick. Call after every delta
/// (`aurix_net_sync_receive` returned 1). 0 on success, -1 on error.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_pump(
    sync: *const AurixNetSync,
    client: *mut AurixNetClient,
) -> i32 {
    let Some(sync) = sync.as_ref() else { return -1 };
    pump(&mut sync.inner.lock(), client)
}

/// Raw input for server-side logic (rooms with `props.logic`): stamped ahead of the last delta by the
/// round trip plus two ticks and resent with the next two inputs. Returns the stamped tick or -1.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_send_input(
    sync: *const AurixNetSync,
    client: *mut AurixNetClient,
    data: *const u8,
    len: usize,
) -> i64 {
    let Some(sync) = sync.as_ref() else { return -1 };
    let body = if data.is_null() || len == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(data, len).to_vec()
    };
    let mut st = sync.inner.lock();
    send_input_inner(&mut st, client, body, false)
}

/// Starts a full write of `component` (fields default to zero until written).
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_write_begin(
    sync: *const AurixNetSync,
    component: i32,
) -> i32 {
    let Some(sync) = sync.as_ref() else { return -1 };
    let mut st = sync.inner.lock();
    let Some(found) = u16::try_from(component)
        .ok()
        .and_then(|id| st.frame.schema.by_id(id))
    else {
        set_error(ErrorCode::InvalidArgument, "write_begin: unknown component");
        return -1;
    };
    let zero = ComponentValue::zero(found);
    st.current = Some((found.id, zero));
    0
}

fn with_field(
    st: &mut State,
    field: i32,
    convert: impl FnOnce(&FieldType) -> Option<Value>,
) -> i32 {
    let Some((cid, cur)) = st.current.as_mut() else {
        set_error(ErrorCode::InvalidArgument, "write: call write_begin first");
        return -1;
    };
    let Some(ty) = st
        .frame
        .schema
        .by_id(*cid)
        .and_then(|found| found.fields.get(usize::try_from(field).ok()?))
        .map(|fd| fd.ty.clone())
    else {
        set_error(ErrorCode::InvalidArgument, "write: unknown field");
        return -1;
    };
    match convert(&ty) {
        Some(value) => {
            cur.fields[field as usize] = value;
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
    sync: *const AurixNetSync,
    field: i32,
    value: f64,
) -> i32 {
    let Some(sync) = sync.as_ref() else { return -1 };
    let mut st = sync.inner.lock();
    with_field(&mut st, field, |ty| match ty {
        FieldType::Bool => Some(Value::Bool(value != 0.0)),
        FieldType::UInt { .. } => Some(Value::UInt(value.max(0.0) as u64)),
        FieldType::Int { .. } => Some(Value::Int(value as i64)),
        FieldType::F32 | FieldType::Quant { .. } => Some(Value::F32(value as f32)),
        _ => None,
    })
}

/// `count` = 2 (vec2), 3 (vec3) or 4 (quat x,y,z,w).
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_write_vector(
    sync: *const AurixNetSync,
    field: i32,
    values: *const f32,
    count: i32,
) -> i32 {
    let Some(sync) = sync.as_ref() else { return -1 };
    if values.is_null() || !(1..=4).contains(&count) {
        set_error(
            ErrorCode::InvalidArgument,
            "write_vector: bad pointer or count",
        );
        return -1;
    }
    let values = std::slice::from_raw_parts(values, count as usize);
    let mut st = sync.inner.lock();
    with_field(&mut st, field, |ty| match (ty, values.len()) {
        (FieldType::Vec2 { .. }, 2) => Some(Value::Vec2([values[0], values[1]])),
        (FieldType::Vec3 { .. }, 3) => Some(Value::Vec3([values[0], values[1], values[2]])),
        (FieldType::Quat, 4) => Some(Value::Quat([values[0], values[1], values[2], values[3]])),
        (FieldType::F32 | FieldType::Quant { .. }, 1) => Some(Value::F32(values[0])),
        _ => None,
    })
}

/// String (UTF-8, invalid bytes replaced) or bytes field.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_write_bytes(
    sync: *const AurixNetSync,
    field: i32,
    data: *const u8,
    len: usize,
) -> i32 {
    let Some(sync) = sync.as_ref() else { return -1 };
    let bytes = if data.is_null() || len == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(data, len).to_vec()
    };
    let mut st = sync.inner.lock();
    with_field(&mut st, field, |ty| match ty {
        FieldType::String { max } => {
            let mut text = String::from_utf8_lossy(&bytes).into_owned();
            while text.len() > *max as usize {
                text.pop();
            }
            Some(Value::String(text))
        }
        FieldType::Bytes { max } => {
            let mut cut = bytes.clone();
            cut.truncate(*max as usize);
            Some(Value::Bytes(cut))
        }
        _ => None,
    })
}

/// Finishes the current component write (queued until `send_writes`).
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_write_end(sync: *const AurixNetSync) -> i32 {
    let Some(sync) = sync.as_ref() else { return -1 };
    let mut st = sync.inner.lock();
    match st.current.take() {
        Some(write) => {
            st.writes.push(write);
            0
        }
        None => {
            set_error(ErrorCode::InvalidArgument, "write_end: nothing begun");
            -1
        }
    }
}

/// Queued writes as `u8 n, n × (cid, encode_full)`; -1 (with error) on a bad write.
fn encode_queued_writes(st: &mut State, writer: &mut BitWriter) -> i32 {
    let writes = std::mem::take(&mut st.writes);
    let cid_bits = st.frame.schema.component_id_bits();
    writer.write_bits(writes.len().min(255) as u64, 8);
    for (cid, value) in writes.iter().take(255) {
        let Some(found) = st.frame.schema.by_id(*cid) else {
            return -1;
        };
        writer.write_bits(u64::from(*cid), cid_bits);
        if let Err(error) = value.encode_full(found, writer) {
            set_error(ErrorCode::InvalidArgument, format!("writes: {error}"));
            return -1;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_send_writes(
    sync: *const AurixNetSync,
    client: *mut AurixNetClient,
) -> i64 {
    let Some(sync) = sync.as_ref() else { return -1 };
    let mut st = sync.inner.lock();
    let mut writer = BitWriter::new();
    if encode_queued_writes(&mut st, &mut writer) != 0 {
        return -1;
    }
    let body = writer.finish();
    send_input_inner(&mut st, client, body, false)
}

/// One owner-authoritative op as this tick's input (rooms with a schema and no `logic`):
/// `op` 0 = write `entity` with the queued writes, 1 = spawn a new entity you own (queued
/// writes are its initial state; at most 32 per actor), 2 = despawn `entity`, 3 = hand `entity`
/// to actor `to`, 4 = set the ownership policy of `entity` to `to` (0 destroy_on_leave, 1 to_master,
/// 2 orphan, 3 dynamic), 5 = request ownership of `entity` (unowned, or policy dynamic), 6 = release
/// `entity` (stays alive, unowned). Only entities you own except 5; the node ignores anything else.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_sync_send_op(
    sync: *const AurixNetSync,
    client: *mut AurixNetClient,
    op: i32,
    entity: u64,
    to: i32,
) -> i64 {
    let Some(sync) = sync.as_ref() else { return -1 };
    if !(0..=6).contains(&op) {
        set_error(ErrorCode::InvalidArgument, "send_op: op must be 0..=6");
        return -1;
    }
    let mut st = sync.inner.lock();
    let mut writer = BitWriter::new();
    writer.write_bits(0xFF, 8);
    writer.write_bits(1, 8);
    writer.write_bits(op as u64, 8);
    if op != 1 {
        writer.write_bits(entity, 48);
    }
    match op {
        0 | 1 => {
            if encode_queued_writes(&mut st, &mut writer) != 0 {
                return -1;
            }
        }
        3 => writer.write_bits(to.clamp(0, 0xFFFF) as u64, 16),
        4 => writer.write_bits(to.clamp(0, 3) as u64, 8),
        _ => {}
    }
    let body = writer.finish();
    send_input_inner(&mut st, client, body, true)
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
            let sync = aurix_net_sync_new(cschema.as_ptr());
            assert!(!sync.is_null());
            let mut ack = [0u8; 9];
            assert_eq!(
                aurix_net_sync_receive(sync, delta.as_ptr(), delta.len(), ack.as_mut_ptr()),
                1
            );
            assert_eq!(ack[0], KIND_ACK);
            assert_eq!(u32::from_be_bytes(ack[1..5].try_into().unwrap()), 12);
            assert_eq!(aurix_net_sync_server_tick(sync), 12);
            let mut ev = AurixNetSyncEvent {
                kind: 0,
                entity: 0,
                owner: 0,
            };
            assert_eq!(aurix_net_sync_poll(sync, &mut ev), 1);
            assert_eq!(
                (ev.kind, ev.entity, ev.owner),
                (AURIX_NET_SYNC_SPAWN, id.to_u64(), 3)
            );
            assert_eq!(aurix_net_sync_poll(sync, &mut ev), 0);
            let tr = aurix_net_sync_component(sync, c"Transform".as_ptr());
            let tag = aurix_net_sync_component(sync, c"Tag".as_ptr());
            assert!(tr >= 0 && tag >= 0);
            assert_eq!(aurix_net_sync_component(sync, c"Nope".as_ptr()), -1);
            let pos = aurix_net_sync_field(sync, tr, c"pos".as_ptr());
            let hp = aurix_net_sync_field(sync, tag, c"hp".as_ptr());
            let name = aurix_net_sync_field(sync, tag, c"name".as_ptr());
            let mut vector = [0f32; 4];
            assert_eq!(
                aurix_net_sync_get_vector(sync, id.to_u64(), tr, pos, vector.as_mut_ptr()),
                3
            );
            assert!(
                (vector[0] - 1.5).abs() < 0.01
                    && (vector[1] + 2.0).abs() < 0.01
                    && (vector[2] - 3.25).abs() < 0.01
            );
            let mut number = 0f64;
            assert_eq!(
                aurix_net_sync_get_number(sync, id.to_u64(), tag, hp, &mut number),
                1
            );
            assert_eq!(number, 90.0);
            let mut buf = [0u8; 16];
            assert_eq!(
                aurix_net_sync_get_bytes(sync, id.to_u64(), tag, name, buf.as_mut_ptr(), 16),
                3
            );
            assert_eq!(&buf[..3], b"bob");
            assert_eq!(aurix_net_sync_entity_count(sync), 1);
            assert_eq!(aurix_net_sync_has(sync, id.to_u64(), tag), 1);

            // a write round-trips through the schema codec
            assert_eq!(aurix_net_sync_write_begin(sync, tr), 0);
            let moved = [4.0f32, 5.0, 6.0];
            assert_eq!(aurix_net_sync_write_vector(sync, pos, moved.as_ptr(), 3), 0);
            assert_eq!(aurix_net_sync_write_number(sync, pos, 1.0), -1);
            assert_eq!(aurix_net_sync_write_end(sync), 0);
            {
                let st = sync.as_ref().unwrap().inner.lock();
                assert_eq!(st.writes.len(), 1);
                assert_eq!(st.writes[0].1.fields[0], Value::Vec3([4.0, 5.0, 6.0]));
            }
            // not a delta
            let junk = [0x20u8, 1, 2, 3, 4, 5, 6, 7, 8, 9];
            assert_eq!(
                aurix_net_sync_receive(sync, junk.as_ptr(), junk.len(), std::ptr::null_mut()),
                0
            );
            aurix_net_sync_destroy(sync);
        }
    }
}
