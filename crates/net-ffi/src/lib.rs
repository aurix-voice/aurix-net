//! C ABI over `net-client`. One global tokio runtime; every call is synchronous and safe to
//! make from a game thread. Events are pulled with `aurix_net_poll` (non-blocking).
//! Strings are UTF-8, NUL-terminated. Header: `include/aurix_net.h`.
#![allow(clippy::missing_safety_doc)]

/// aurix-sim (deterministic physics 2D/3D, navmesh, crowd) is linked into this library too: its
/// `sim_*`/`sim3_*` C ABI is declared in `include/aurix_sim.h`. The re-export keeps the crate
/// linked so the `#[no_mangle]` symbols end up in the cdylib/staticlib.
pub use net_sim_wasm;

mod sigpipe;
/// `aurix_net_sync_*`: client-side replicated world for sync rooms.
pub mod sync;

use bytes::Bytes;
use net_client::protocol::{ClientMsg, ServerMsg};
use net_client::{CacheOp, Client, ClientError, Event, EventFrame, EventTarget, Options};
use net_common::{ActorId, Channel};
use parking_lot::Mutex;
use std::cell::{Cell, RefCell};
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("aurix-net")
            .on_thread_start(sigpipe::block)
            .build()
            .expect("tokio runtime")
    })
}

/// Runs a call of the game to its end on the game's thread.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let _sigpipe = sigpipe::Blocked::new();
    rt().block_on(future)
}

/// The panic that stopped a handle.
#[derive(Default)]
pub(crate) struct Stopped {
    reason: OnceLock<String>,
    told: AtomicBool,
}

impl Stopped {
    /// What a stopped client gives its poll: a `closed` event with the panic once, then
    /// nothing.
    fn poll(&self, out: &mut AurixNetEvent) -> Option<i32> {
        let reason = self.reason.get()?;
        if self.told.swap(true, Ordering::Relaxed) {
            return Some(0);
        }
        out.kind = AURIX_NET_EVENT_CLOSED;
        out.json = CString::new(reason.as_str())
            .map(CString::into_raw)
            .unwrap_or(std::ptr::null_mut());
        Some(1)
    }
}

pub(crate) trait Handle {
    fn stopped(&self) -> &Stopped;
}

pub(crate) unsafe fn stopped<'a, H: Handle + 'a>(handle: *const H) -> Option<&'a Stopped> {
    handle.as_ref().map(Handle::stopped)
}

/// Runs the body of a function of the C interface. A panic that left it would end the process
/// of the game, so it is caught: the call returns `failed` with `AURIX_NET_ERR_PANIC`, and the
/// handles it worked on are stopped, since what the panic left in them is unknown.
pub(crate) fn run<T>(handles: &[Option<&Stopped>], failed: T, body: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("unknown");
            let reason = format!("panic: {message}");
            for handle in handles.iter().flatten() {
                handle.reason.get_or_init(|| reason.clone());
            }
            set_error(ErrorCode::Panic, reason);
            failed
        }
    }
}

/// `run` for every call but the one that frees the handle: a stopped handle fails the call with
/// the panic that stopped it.
pub(crate) fn guard<T>(handles: &[Option<&Stopped>], failed: T, body: impl FnOnce() -> T) -> T {
    if let Some(reason) = handles
        .iter()
        .flatten()
        .find_map(|handle| handle.reason.get())
    {
        set_error(ErrorCode::Panic, reason);
        return failed;
    }
    run(handles, failed, body)
}

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
    static LAST_CODE: Cell<i32> = const { Cell::new(ErrorCode::Ok as i32) };
}

/// Why a call failed: the `AURIX_NET_ERR_*` constants of `aurix_net.h`.
#[derive(Clone, Copy)]
#[repr(i32)]
pub(crate) enum ErrorCode {
    Ok = 0,
    InvalidArgument = 1,
    Http = 2,
    Connection = 3,
    DataPath = 4,
    Disconnected = 5,
    Timeout = 6,
    Protocol = 7,
    Panic = 8,
    Unauthorized = 100,
    Forbidden = 101,
    NotFound = 102,
    Conflict = 103,
    InvalidRequest = 104,
    RoomFull = 105,
    RoomClosed = 106,
    SchemaMismatch = 107,
    QuotaExceeded = 108,
    RateLimited = 109,
    Left = 110,
    Server = 199,
}

impl From<&ClientError> for ErrorCode {
    fn from(error: &ClientError) -> Self {
        match error {
            ClientError::Http(_) => ErrorCode::Http,
            ClientError::Ws(_) => ErrorCode::Connection,
            ClientError::Data(_) => ErrorCode::DataPath,
            ClientError::Disconnected | ClientError::Closed(_) => ErrorCode::Disconnected,
            ClientError::TooLarge(..) => ErrorCode::InvalidArgument,
            ClientError::Timeout => ErrorCode::Timeout,
            ClientError::NotJoin => ErrorCode::InvalidRequest,
            ClientError::Left(_) => ErrorCode::Left,
            ClientError::Server { code, .. } => match code.as_str() {
                "unauthorized" => ErrorCode::Unauthorized,
                "forbidden" => ErrorCode::Forbidden,
                "not_found" => ErrorCode::NotFound,
                "conflict" => ErrorCode::Conflict,
                "invalid_request" => ErrorCode::InvalidRequest,
                "room_full" => ErrorCode::RoomFull,
                "room_closed" => ErrorCode::RoomClosed,
                "schema_mismatch" => ErrorCode::SchemaMismatch,
                "quota_exceeded" => ErrorCode::QuotaExceeded,
                "rate_limited" => ErrorCode::RateLimited,
                _ => ErrorCode::Server,
            },
        }
    }
}

pub(crate) fn set_error(code: ErrorCode, error: impl std::fmt::Display) {
    let text = CString::new(error.to_string().replace('\0', " ")).unwrap_or_default();
    let _ = LAST_ERROR.try_with(|last| *last.borrow_mut() = text);
    let _ = LAST_CODE.try_with(|last| last.set(code as i32));
}

pub(crate) fn set_client_error(error: &ClientError) {
    set_error(error.into(), error);
}

unsafe fn cstr<'a>(pointer: *const c_char) -> Option<&'a str> {
    if pointer.is_null() {
        return None;
    }
    CStr::from_ptr(pointer).to_str().ok()
}

/// The control message a game wrote as JSON. One the node could not read is refused here with
/// the node's code, `invalid_request`, and what is wrong with it: a field the node does not
/// know is named.
fn control_message(json: &str) -> Option<ClientMsg> {
    match serde_json::from_str(json) {
        Ok(msg) => Some(msg),
        Err(error) => {
            set_error(
                ErrorCode::InvalidRequest,
                format_args!("bad message: {error}"),
            );
            None
        }
    }
}

pub struct AurixNetClient {
    inner: Mutex<Client>,
    stopped: Stopped,
}

impl Handle for AurixNetClient {
    fn stopped(&self) -> &Stopped {
        &self.stopped
    }
}

/// Event kinds returned by `aurix_net_poll`.
pub const AURIX_NET_EVENT_NONE: i32 = 0;
/// `json` holds the server control message (same JSON as the WebSocket protocol).
pub const AURIX_NET_EVENT_CONTROL: i32 = 1;
/// `data` holds a data-path payload on `channel`.
pub const AURIX_NET_EVENT_DATA: i32 = 2;
pub const AURIX_NET_EVENT_DATA_READY: i32 = 3;
/// `json` holds the reason.
pub const AURIX_NET_EVENT_CLOSED: i32 = 4;
/// `data` holds the reliable events the node had not acknowledged when the room was lost;
/// read them with `aurix_net_undelivered_count` and `aurix_net_undelivered_get`.
pub const AURIX_NET_EVENT_UNDELIVERED: i32 = 5;

/// One event of an `AURIX_NET_EVENT_UNDELIVERED`, as `aurix_net_send_event_ex` was given it.
/// `data` is valid until the event is freed.
#[repr(C)]
pub struct AurixNetUndelivered {
    pub code: u8,
    pub target: i32,
    pub n_actors: usize,
    pub data: *const u8,
    pub len: usize,
    /// -1 when the event had no interest group.
    pub group: i32,
    pub cache: i32,
}

/// The frames of an `AURIX_NET_EVENT_UNDELIVERED`, each after its length.
fn undelivered_frames(events: &[EventFrame]) -> Box<[u8]> {
    let mut frames = Vec::new();
    for event in events {
        let frame = event.encode();
        frames.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        frames.extend_from_slice(&frame);
    }
    frames.into_boxed_slice()
}

/// The frame at `index` of an `AURIX_NET_EVENT_UNDELIVERED`.
unsafe fn undelivered_frame<'a>(ev: *const AurixNetEvent, index: usize) -> Option<&'a [u8]> {
    let ev = ev.as_ref()?;
    if ev.kind != AURIX_NET_EVENT_UNDELIVERED || ev.data.is_null() {
        return None;
    }
    let mut rest = std::slice::from_raw_parts(ev.data, ev.data_len);
    let mut frames = std::iter::from_fn(|| {
        let (len, after) = rest.split_first_chunk::<4>()?;
        let (frame, after) = after.split_at_checked(u32::from_le_bytes(*len) as usize)?;
        rest = after;
        Some(frame)
    });
    frames.nth(index)
}

/// How many events an `AURIX_NET_EVENT_UNDELIVERED` holds; 0 for any other event.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_undelivered_count(ev: *const AurixNetEvent) -> usize {
    guard(&[], 0, || {
        (0..)
            .take_while(|&index| undelivered_frame(ev, index).is_some())
            .count()
    })
}

/// Fills `out` with the event at `index` of an `AURIX_NET_EVENT_UNDELIVERED`, in the order the
/// events were sent. Returns 0, or -1 when there is no such event.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_undelivered_get(
    ev: *const AurixNetEvent,
    index: usize,
    out: *mut AurixNetUndelivered,
) -> i32 {
    guard(&[], -1, || {
        let (Some(frame), Some(out)) = (undelivered_frame(ev, index), out.as_mut()) else {
            return -1;
        };
        let Ok(event) = EventFrame::decode(Bytes::copy_from_slice(frame)) else {
            return -1;
        };
        let payload = &frame[frame.len() - event.payload.len()..];
        let (target, n_actors) = match &event.target {
            EventTarget::All => (0, 0),
            EventTarget::Others => (1, 0),
            EventTarget::Master => (2, 0),
            EventTarget::Actors(actors) => (3, actors.len()),
        };
        *out = AurixNetUndelivered {
            code: event.code,
            target,
            n_actors,
            data: payload.as_ptr(),
            len: payload.len(),
            group: event.group.map_or(-1, i32::from),
            cache: match event.cache {
                CacheOp::None => 0,
                CacheOp::Add => 1,
                CacheOp::Remove => 2,
            },
        };
        0
    })
}

/// Copies the actors the event at `index` was sent to into `out`, at most `cap` of them.
/// Returns how many it copied.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_undelivered_actors(
    ev: *const AurixNetEvent,
    index: usize,
    out: *mut u16,
    cap: usize,
) -> usize {
    guard(&[], 0, || {
        let decoded = undelivered_frame(ev, index)
            .and_then(|frame| EventFrame::decode(Bytes::copy_from_slice(frame)).ok());
        let Some(EventTarget::Actors(actors)) = decoded.map(|event| event.target) else {
            return 0;
        };
        if out.is_null() {
            return 0;
        }
        let copied = actors.len().min(cap);
        for (slot, actor) in actors.iter().take(copied).enumerate() {
            out.add(slot).write_unaligned(actor.0);
        }
        copied
    })
}

#[repr(C)]
pub struct AurixNetEvent {
    pub kind: i32,
    pub channel: u8,
    pub tick: u16,
    pub data: *mut u8,
    pub data_len: usize,
    pub json: *mut c_char,
}

impl AurixNetEvent {
    fn empty() -> Self {
        Self {
            kind: AURIX_NET_EVENT_NONE,
            channel: 0,
            tick: 0,
            data: std::ptr::null_mut(),
            data_len: 0,
            json: std::ptr::null_mut(),
        }
    }
}

#[no_mangle]
pub extern "C" fn aurix_net_version() -> *const c_char {
    static VERSION: &[u8] = b"0.1.0\0";
    VERSION.as_ptr() as *const c_char
}

/// Last error message for the calling thread (valid until the next failing call).
#[no_mangle]
pub extern "C" fn aurix_net_last_error() -> *const c_char {
    LAST_ERROR
        .try_with(|last| last.borrow().as_ptr())
        .unwrap_or(c"".as_ptr())
}

/// Why the last failed call on this thread failed: one of the `AURIX_NET_ERR_*` constants.
/// Set together with `aurix_net_last_error` by a call that reports failure; a call that
/// succeeds leaves both as they were.
#[no_mangle]
pub extern "C" fn aurix_net_last_error_code() -> i32 {
    LAST_CODE
        .try_with(Cell::get)
        .unwrap_or(ErrorCode::Panic as i32)
}

/// Makes `code` and `message` the last error of the calling thread. The error is kept per
/// thread, so a wrapper that runs a call on a worker thread and reports its failure on another
/// carries it over with this. A NULL `message` leaves an empty text.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_set_last_error(code: i32, message: *const c_char) {
    guard(&[], (), || {
        let text = CString::new(cstr(message).unwrap_or_default()).unwrap_or_default();
        let _ = LAST_ERROR.try_with(|last| *last.borrow_mut() = text);
        let _ = LAST_CODE.try_with(|last| last.set(code));
    })
}

/// Backend/tooling helper: issues a grant with an API key. Returns a malloc'd string to free
/// with `aurix_net_string_free`, or NULL. Never call this from a shipped game client.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_issue_grant(
    http: *const c_char,
    api_key: *const c_char,
    player_id: *const c_char,
    name: *const c_char,
) -> *mut c_char {
    guard(&[], std::ptr::null_mut(), || {
        let (Some(http), Some(key), Some(pid), Some(name)) =
            (cstr(http), cstr(api_key), cstr(player_id), cstr(name))
        else {
            set_error(ErrorCode::InvalidArgument, "null or non-utf8 argument");
            return std::ptr::null_mut();
        };
        match block_on(net_client::issue_grant(http, key, pid, name)) {
            Ok(grant) => CString::new(grant)
                .map(CString::into_raw)
                .unwrap_or(std::ptr::null_mut()),
            Err(error) => {
                set_client_error(&error);
                std::ptr::null_mut()
            }
        }
    })
}

/// Region list and best-region ping: JSON `{region, endpoint, rtt_ms, candidates:[...]}` or NULL
/// (see aurix_net_last_error). Blocks for up to a few seconds; free with aurix_net_string_free.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_best_region(
    catalog_http: *const c_char,
    probes: i32,
) -> *mut c_char {
    guard(&[], std::ptr::null_mut(), || {
        let Ok(url) = std::ffi::CStr::from_ptr(catalog_http).to_str() else {
            set_error(ErrorCode::InvalidArgument, "best_region: bad url");
            return std::ptr::null_mut();
        };
        match block_on(net_client::best_region(url, probes.clamp(1, 10) as u8)) {
            Ok(pick) => CString::new(serde_json::to_string(&pick).unwrap_or_default())
                .map(CString::into_raw)
                .unwrap_or(std::ptr::null_mut()),
            Err(error) => {
                set_client_error(&error);
                std::ptr::null_mut()
            }
        }
    })
}
/// A top-level string field of a JSON object, e.g. `t` of a control message. Returns a malloc'd
/// string to free with `aurix_net_string_free`, or NULL when the field is absent or not a string.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_json_string(
    json: *const c_char,
    key: *const c_char,
) -> *mut c_char {
    guard(&[], std::ptr::null_mut(), || {
        let (Some(json), Some(key)) = (cstr(json), cstr(key)) else {
            return std::ptr::null_mut();
        };
        serde_json::from_str::<serde_json::Value>(json)
            .ok()
            .and_then(|value| {
                value
                    .get(key)?
                    .as_str()
                    .and_then(|text| CString::new(text).ok())
            })
            .map(CString::into_raw)
            .unwrap_or(std::ptr::null_mut())
    })
}
/// The text of `json` written anew when it is one JSON value, e.g. the props or the filter a
/// game hands over as text: what goes into a message is then that value and nothing beside it.
/// Returns a string to free with `aurix_net_string_free`, or NULL when it is anything else;
/// the error then says so of `name`, the part of the message the text was to be.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_json_value(
    name: *const c_char,
    json: *const c_char,
) -> *mut c_char {
    guard(&[], std::ptr::null_mut(), || {
        let (Some(name), Some(json)) = (cstr(name), cstr(json)) else {
            return std::ptr::null_mut();
        };
        match serde_json::from_str::<serde_json::Value>(json) {
            Ok(value) => CString::new(value.to_string())
                .map(CString::into_raw)
                .unwrap_or(std::ptr::null_mut()),
            Err(error) => {
                set_error(
                    ErrorCode::InvalidRequest,
                    format_args!("bad message: {name}: {error}"),
                );
                std::ptr::null_mut()
            }
        }
    })
}
#[no_mangle]
pub unsafe extern "C" fn aurix_net_string_free(string: *mut c_char) {
    guard(&[], (), || {
        if !string.is_null() {
            drop(CString::from_raw(string));
        }
    })
}

/// Opens the control channel and authenticates. `tunnel != 0` forces the WebSocket tunnel.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_connect(
    http: *const c_char,
    grant: *const c_char,
    tunnel: i32,
) -> *mut AurixNetClient {
    guard(&[], std::ptr::null_mut(), || {
        let (Some(http), Some(grant)) = (cstr(http), cstr(grant)) else {
            set_error(ErrorCode::InvalidArgument, "null or non-utf8 argument");
            return std::ptr::null_mut();
        };
        let opts = Options {
            tunnel: tunnel != 0,
            ..Default::default()
        };
        match block_on(Client::connect(http, grant, opts)) {
            Ok(inner) => Box::into_raw(Box::new(AurixNetClient {
                inner: Mutex::new(inner),
                stopped: Stopped::default(),
            })),
            Err(error) => {
                set_client_error(&error);
                std::ptr::null_mut()
            }
        }
    })
}

/// `aurix_net_connect` and `aurix_net_join` in one call that follows the node's answer when the
/// room lives on another node of the region: the client it returns is connected to that node
/// and is in the room. `actor` (may be NULL) receives the actor id. NULL on failure.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_connect_and_join(
    http: *const c_char,
    grant: *const c_char,
    tunnel: i32,
    json: *const c_char,
    actor: *mut i32,
) -> *mut AurixNetClient {
    guard(&[], std::ptr::null_mut(), || {
        let (Some(http), Some(grant), Some(json)) = (cstr(http), cstr(grant), cstr(json)) else {
            set_error(ErrorCode::InvalidArgument, "null or non-utf8 argument");
            return std::ptr::null_mut();
        };
        let Some(msg) = control_message(json) else {
            return std::ptr::null_mut();
        };
        let opts = Options {
            tunnel: tunnel != 0,
            ..Default::default()
        };
        match block_on(Client::connect_and_join(http, grant, opts, msg)) {
            Ok((inner, joined)) => {
                if let Some(actor) = actor.as_mut() {
                    *actor = joined.actor.0 as i32;
                }
                Box::into_raw(Box::new(AurixNetClient {
                    inner: Mutex::new(inner),
                    stopped: Stopped::default(),
                }))
            }
            Err(error) => {
                set_client_error(&error);
                std::ptr::null_mut()
            }
        }
    })
}

/// Sends a control message given as protocol JSON (`{"t":"set_room_props",...}`). 0 on success.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_send_control(
    client: *mut AurixNetClient,
    json: *const c_char,
) -> i32 {
    guard(&[stopped(client)], -1, || {
        let (Some(client), Some(json)) = (client.as_ref(), cstr(json)) else {
            return -1;
        };
        let Some(msg) = control_message(json) else {
            return -1;
        };
        match block_on(client.inner.lock().send(msg)) {
            Ok(()) => 0,
            Err(error) => {
                set_client_error(&error);
                -1
            }
        }
    })
}

/// Joins/creates a room with a protocol JSON message (`{"t":"join_or_create","name":...}`),
/// opens the data path and blocks until it is ready. Returns the actor id, or -1.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_join(client: *mut AurixNetClient, json: *const c_char) -> i32 {
    guard(&[stopped(client)], -1, || {
        let (Some(client), Some(json)) = (client.as_ref(), cstr(json)) else {
            return -1;
        };
        let Some(msg) = control_message(json) else {
            return -1;
        };
        let mut inner = client.inner.lock();
        match block_on(inner.join(msg)) {
            Ok(joined) => joined.actor.0 as i32,
            Err(error) => {
                set_client_error(&error);
                -1
            }
        }
    })
}

/// What the node said of the room at the join, as JSON, malloc'd; NULL when not in a room:
/// `{"room","name","mode","actor","master","max_players","props","players","tick_hz","schema_hash",
/// "tick","seed","next_spawn_tag"}`. `players` are those in the room at the join, this client among
/// them; `player_joined` and `player_left` tell of the later ones.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_joined_json(client: *mut AurixNetClient) -> *mut c_char {
    guard(&[stopped(client)], std::ptr::null_mut(), || {
        let Some(client) = client.as_ref() else {
            return std::ptr::null_mut();
        };
        let inner = client.inner.lock();
        let Some(joined) = &inner.joined else {
            return std::ptr::null_mut();
        };
        let facts = serde_json::json!({
            "room": joined.room, "name": joined.name, "mode": joined.mode, "actor": joined.actor.0,
            "master": joined.master.0, "max_players": joined.max_players, "props": joined.props,
            "players": joined.players, "tick_hz": joined.tick_hz, "schema_hash": joined.schema_hash,
            "tick": joined.tick, "seed": joined.seed, "next_spawn_tag": joined.next_spawn_tag,
        });
        CString::new(facts.to_string())
            .map(CString::into_raw)
            .unwrap_or(std::ptr::null_mut())
    })
}

/// The player id of the grant the client signed in with: the id the node knows the player by in
/// rooms, held seats and webhooks. Free with [`aurix_net_string_free`].
#[no_mangle]
pub unsafe extern "C" fn aurix_net_player_id(client: *mut AurixNetClient) -> *mut c_char {
    guard(&[stopped(client)], std::ptr::null_mut(), || {
        let Some(client) = client.as_ref() else {
            return std::ptr::null_mut();
        };
        CString::new(client.inner.lock().player_id())
            .map(CString::into_raw)
            .unwrap_or(std::ptr::null_mut())
    })
}

/// The display name of the grant the client signed in with. Free with [`aurix_net_string_free`].
#[no_mangle]
pub unsafe extern "C" fn aurix_net_player_name(client: *mut AurixNetClient) -> *mut c_char {
    guard(&[stopped(client)], std::ptr::null_mut(), || {
        let Some(client) = client.as_ref() else {
            return std::ptr::null_mut();
        };
        CString::new(client.inner.lock().player_name())
            .map(CString::into_raw)
            .unwrap_or(std::ptr::null_mut())
    })
}

/// The seed of the lockstep room the client is in: every client and the node start the game
/// from it. Returns 0, or -1 when the client is not in a lockstep room.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_joined_seed(client: *mut AurixNetClient, seed: *mut u64) -> i32 {
    guard(&[stopped(client)], -1, || {
        let (Some(client), Some(seed)) = (client.as_ref(), seed.as_mut()) else {
            return -1;
        };
        match client
            .inner
            .lock()
            .joined
            .as_ref()
            .and_then(|joined| joined.seed)
        {
            Some(value) => {
                *seed = value;
                0
            }
            None => -1,
        }
    })
}

/// The entity a sync room made for the client as its avatar. The client keeps it from the join
/// on, so it answers whenever the game asks. Returns 1 = `entity` filled, 0 = the room has not
/// announced it.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_avatar(client: *mut AurixNetClient, entity: *mut u64) -> i32 {
    guard(&[stopped(client)], -1, || {
        let (Some(client), Some(entity)) = (client.as_ref(), entity.as_mut()) else {
            return -1;
        };
        match client.inner.lock().avatar {
            Some(avatar) => {
                *entity = avatar;
                1
            }
            None => 0,
        }
    })
}

/// Raw data-path message. channel: 0 unreliable-sequenced, 1 reliable-ordered, 2 reliable-unordered,
/// 4 unreliable-unsequenced.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_send_data(
    client: *mut AurixNetClient,
    channel: u8,
    data: *const u8,
    len: usize,
) -> i32 {
    guard(&[stopped(client)], -1, || {
        let Some(client) = client.as_ref() else {
            return -1;
        };
        let Some(ch) = Channel::from_u8(channel) else {
            set_error(ErrorCode::InvalidArgument, "bad channel");
            return -1;
        };
        let payload = if data.is_null() || len == 0 {
            Bytes::new()
        } else {
            Bytes::copy_from_slice(std::slice::from_raw_parts(data, len))
        };
        match client.inner.lock().send_data(ch, payload) {
            Ok(()) => 0,
            Err(error) => {
                set_client_error(&error);
                -1
            }
        }
    })
}

/// Relay event. target: 0 all, 1 others, 2 master, 3 explicit `actors[n]`.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_send_event(
    client: *mut AurixNetClient,
    code: u8,
    target: i32,
    actors: *const u16,
    n_actors: usize,
    data: *const u8,
    len: usize,
    reliable: i32,
) -> i32 {
    guard(&[stopped(client)], -1, || {
        let Some(client) = client.as_ref() else {
            return -1;
        };
        let target = match target {
            0 => EventTarget::All,
            1 => EventTarget::Others,
            2 => EventTarget::Master,
            3 if !actors.is_null() => EventTarget::Actors(
                std::slice::from_raw_parts(actors, n_actors)
                    .iter()
                    .map(|actor| ActorId(*actor))
                    .collect(),
            ),
            _ => {
                set_error(ErrorCode::InvalidArgument, "bad target");
                return -1;
            }
        };
        let payload = if data.is_null() || len == 0 {
            Bytes::new()
        } else {
            Bytes::copy_from_slice(std::slice::from_raw_parts(data, len))
        };
        match client
            .inner
            .lock()
            .send_event(code, target, payload, reliable != 0)
        {
            Ok(()) => 0,
            Err(error) => {
                set_client_error(&error);
                -1
            }
        }
    })
}
/// `aurix_net_send_event` plus `group` (interest group, -1 = none) and `cache` (0 none, 1 add to the room
/// cache for late joiners, 2 remove).
#[no_mangle]
pub unsafe extern "C" fn aurix_net_send_event_ex(
    client: *mut AurixNetClient,
    code: u8,
    target: i32,
    actors: *const u16,
    n_actors: usize,
    data: *const u8,
    len: usize,
    reliable: i32,
    group: i32,
    cache: i32,
) -> i32 {
    guard(&[stopped(client)], -1, || {
        aurix_net_send_event_on(
            client,
            if reliable != 0 { 1 } else { 0 },
            code,
            target,
            actors,
            n_actors,
            data,
            len,
            group,
            cache,
        )
    })
}
/// `aurix_net_send_event_ex` on a channel of the caller's choice, numbered as in `aurix_net_send_data`:
/// 0 for state, where a newer event replaces an older one, 4 for events that are all wanted. A node
/// that does not take channel 4 gets its events on channel 0.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_send_event_on(
    client: *mut AurixNetClient,
    channel: u8,
    code: u8,
    target: i32,
    actors: *const u16,
    n_actors: usize,
    data: *const u8,
    len: usize,
    group: i32,
    cache: i32,
) -> i32 {
    guard(&[stopped(client)], -1, || {
        let Some(client) = client.as_ref() else {
            return -1;
        };
        let Some(channel) = Channel::from_u8(channel) else {
            set_error(ErrorCode::InvalidArgument, "bad channel");
            return -1;
        };
        let target = match target {
            0 => EventTarget::All,
            1 => EventTarget::Others,
            2 => EventTarget::Master,
            3 if !actors.is_null() => EventTarget::Actors(
                std::slice::from_raw_parts(actors, n_actors)
                    .iter()
                    .map(|actor| ActorId(*actor))
                    .collect(),
            ),
            _ => {
                set_error(ErrorCode::InvalidArgument, "bad target");
                return -1;
            }
        };
        let group = match group {
            -1 => None,
            0..=0xFFFF => Some(group as u16),
            _ => {
                set_error(ErrorCode::InvalidArgument, "bad group");
                return -1;
            }
        };
        let cache = match cache {
            0 => CacheOp::None,
            1 => CacheOp::Add,
            2 => CacheOp::Remove,
            _ => {
                set_error(ErrorCode::InvalidArgument, "bad cache");
                return -1;
            }
        };
        let payload = if data.is_null() || len == 0 {
            Bytes::new()
        } else {
            Bytes::copy_from_slice(std::slice::from_raw_parts(data, len))
        };
        match client
            .inner
            .lock()
            .send_event_on(channel, code, target, payload, group, cache)
        {
            Ok(()) => 0,
            Err(error) => {
                set_client_error(&error);
                -1
            }
        }
    })
}

/// Pushes queued unreliable messages onto the wire; call once per frame after sending.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_flush(client: *mut AurixNetClient) {
    guard(&[stopped(client)], (), || {
        if let Some(client) = client.as_ref() {
            client.inner.lock().flush();
        }
    })
}

/// Non-blocking: fills `out` and returns 1 when an event was available, 0 otherwise.
/// Free the event with `aurix_net_event_free`. A client a panic stopped hands out one last
/// `closed` event with the panic as its reason.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_poll(
    client: *mut AurixNetClient,
    out: *mut AurixNetEvent,
) -> i32 {
    run(&[stopped(client)], 0, || {
        let (Some(client), Some(out)) = (client.as_ref(), out.as_mut()) else {
            return 0;
        };
        *out = AurixNetEvent::empty();
        if let Some(answer) = client.stopped.poll(out) {
            return answer;
        }
        let ev = client.inner.lock().try_next_event();
        let Some(ev) = ev else { return 0 };
        match ev {
            Event::Undelivered(events) => {
                out.kind = AURIX_NET_EVENT_UNDELIVERED;
                let mut frames = undelivered_frames(&events);
                out.data_len = frames.len();
                out.data = frames.as_mut_ptr();
                std::mem::forget(frames);
            }
            Event::Control(msg) => {
                out.kind = AURIX_NET_EVENT_CONTROL;
                out.json = json_c(&msg);
            }
            Event::Data {
                channel,
                tick,
                payload,
            } => {
                out.kind = AURIX_NET_EVENT_DATA;
                out.channel = channel as u8;
                out.tick = tick;
                let mut bytes = payload.to_vec().into_boxed_slice();
                out.data_len = bytes.len();
                out.data = bytes.as_mut_ptr();
                std::mem::forget(bytes);
            }
            Event::DataReady => out.kind = AURIX_NET_EVENT_DATA_READY,
            Event::Closed(reason) => {
                out.kind = AURIX_NET_EVENT_CLOSED;
                out.json = CString::new(reason)
                    .map(CString::into_raw)
                    .unwrap_or(std::ptr::null_mut());
            }
        }
        1
    })
}

fn json_c(msg: &ServerMsg) -> *mut c_char {
    serde_json::to_string(msg)
        .ok()
        .and_then(|text| CString::new(text).ok())
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_event_free(ev: *mut AurixNetEvent) {
    guard(&[], (), || {
        let Some(ev) = ev.as_mut() else { return };
        if !ev.data.is_null() {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                ev.data,
                ev.data_len,
            )));
        }
        if !ev.json.is_null() {
            drop(CString::from_raw(ev.json));
        }
        *ev = AurixNetEvent::empty();
    })
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_leave(client: *mut AurixNetClient) -> i32 {
    guard(&[stopped(client)], -1, || {
        let Some(client) = client.as_ref() else {
            return -1;
        };
        let mut inner = client.inner.lock();
        match block_on(inner.leave()) {
            Ok(()) => 0,
            Err(error) => {
                set_client_error(&error);
                -1
            }
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_destroy(client: *mut AurixNetClient) {
    run(&[], (), || {
        if !client.is_null() {
            drop(Box::from_raw(client));
        }
    })
}

// ---- lockstep rooms: a pure-logic helper next to the client (see net_client::lockstep) ----

use net_client::lockstep::{LockstepClient, LsEvent, Outgoing, LS_BUNDLE, MAX_COMMAND, MAX_INPUT};
use std::collections::VecDeque;

pub struct AurixNetLockstep {
    inner: Mutex<LockstepClient>,
    queue: Mutex<VecDeque<LsEvent>>,
    /// The event last handed out by poll: its inputs/state stay readable until the next poll.
    current: Mutex<Option<LsEvent>>,
    stopped: Stopped,
}

impl Handle for AurixNetLockstep {
    fn stopped(&self) -> &Stopped {
        &self.stopped
    }
}

pub const AURIX_NET_LS_NONE: i32 = 0;
pub const AURIX_NET_LS_TICK: i32 = 1;
pub const AURIX_NET_LS_RESTORE: i32 = 2;
pub const AURIX_NET_LS_CHECKPOINT_REQ: i32 = 3;
pub const AURIX_NET_LS_DUMP_REQ: i32 = 4;

#[repr(C)]
pub struct AurixNetLsEvent {
    pub kind: i32,
    pub tick: u32,
    pub hash: u64,
    /// TICK: number of actors with input (read them with aurix_net_lockstep_input).
    pub inputs: u16,
    /// RESTORE: checkpoint state, valid until the next poll.
    pub state: *const u8,
    pub state_len: usize,
}

#[no_mangle]
pub extern "C" fn aurix_net_lockstep_new() -> *mut AurixNetLockstep {
    guard(&[], std::ptr::null_mut(), || {
        Box::into_raw(Box::new(AurixNetLockstep {
            inner: Mutex::new(LockstepClient::new()),
            queue: Mutex::new(VecDeque::new()),
            current: Mutex::new(None),
            stopped: Stopped::default(),
        }))
    })
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_lockstep_destroy(ls: *mut AurixNetLockstep) {
    run(&[], (), || {
        if !ls.is_null() {
            drop(Box::from_raw(ls));
        }
    })
}

/// Feeds one data-path payload. 1 = lockstep frame (events may be queued), 0 = not a lockstep frame.
/// A frame of bundles is acknowledged through `client`, so the room resends the ones that were lost;
/// with a null `client` (a replay) nothing is sent.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_lockstep_receive(
    ls: *mut AurixNetLockstep,
    client: *mut AurixNetClient,
    data: *const u8,
    len: usize,
) -> i32 {
    guard(&[stopped(ls), stopped(client)], -1, || {
        let Some(ls) = ls.as_ref() else { return -1 };
        if data.is_null() || len == 0 {
            return 0;
        }
        let frame = std::slice::from_raw_parts(data, len);
        let ack = {
            let mut lockstep = ls.inner.lock();
            match lockstep.receive(frame) {
                Some(events) => {
                    ls.queue.lock().extend(events);
                    (frame[0] == LS_BUNDLE).then(|| lockstep.ack_frame())
                }
                None => return 0,
            }
        };
        if let Some(ack) = ack {
            if !client.is_null() {
                send_frame(client, ack);
            }
        }
        1
    })
}

/// Pops the next lockstep event; 1 = `out` filled, 0 = none.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_lockstep_poll(
    ls: *mut AurixNetLockstep,
    out: *mut AurixNetLsEvent,
) -> i32 {
    guard(&[stopped(ls)], -1, || {
        let (Some(ls), Some(out)) = (ls.as_ref(), out.as_mut()) else {
            return -1;
        };
        let Some(ev) = ls.queue.lock().pop_front() else {
            out.kind = AURIX_NET_LS_NONE;
            return 0;
        };
        *out = match &ev {
            LsEvent::Tick { tick, inputs } => AurixNetLsEvent {
                kind: AURIX_NET_LS_TICK,
                tick: *tick,
                hash: 0,
                inputs: inputs.len() as u16,
                state: std::ptr::null(),
                state_len: 0,
            },
            LsEvent::Restore { tick, hash, state } => AurixNetLsEvent {
                kind: AURIX_NET_LS_RESTORE,
                tick: *tick,
                hash: *hash,
                inputs: 0,
                state: state.as_ptr(),
                state_len: state.len(),
            },
            LsEvent::CheckpointRequested(tick) => AurixNetLsEvent {
                kind: AURIX_NET_LS_CHECKPOINT_REQ,
                tick: *tick,
                hash: 0,
                inputs: 0,
                state: std::ptr::null(),
                state_len: 0,
            },
            LsEvent::DumpRequested(tick) => AurixNetLsEvent {
                kind: AURIX_NET_LS_DUMP_REQ,
                tick: *tick,
                hash: 0,
                inputs: 0,
                state: std::ptr::null(),
                state_len: 0,
            },
        };
        *ls.current.lock() = Some(ev);
        1
    })
}

/// Reads input `index` of the TICK event last returned by poll (actor ascending). 1 = filled.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_lockstep_input(
    ls: *mut AurixNetLockstep,
    index: u16,
    actor: *mut u16,
    data: *mut *const u8,
    len: *mut usize,
) -> i32 {
    guard(&[stopped(ls)], -1, || {
        let Some(ls) = ls.as_ref() else { return -1 };
        let cur = ls.current.lock();
        let Some(LsEvent::Tick { inputs, .. }) = cur.as_ref() else {
            return 0;
        };
        let Some((input_actor, input)) = inputs.iter().nth(index as usize) else {
            return 0;
        };
        if let Some(actor) = actor.as_mut() {
            *actor = *input_actor;
        }
        if let Some(data) = data.as_mut() {
            *data = input.as_ptr();
        }
        if let Some(len) = len.as_mut() {
            *len = input.len();
        }
        1
    })
}

unsafe fn send_frame(client: *mut AurixNetClient, frame: Outgoing) -> i32 {
    let Some(client) = client.as_ref() else {
        return -1;
    };
    let Some(channel) = Channel::from_u8(if frame.reliable { 1 } else { 0 }) else {
        return -1;
    };
    match client.inner.lock().send_data(channel, frame.payload) {
        Ok(()) => 0,
        Err(error) => {
            set_client_error(&error);
            -1
        }
    }
}

/// Queues this tick's local input and sends it (with the previous ones for redundancy) through
/// `client`. Returns the tick it was stamped with, or -1.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_lockstep_send_input(
    ls: *mut AurixNetLockstep,
    client: *mut AurixNetClient,
    data: *const u8,
    len: usize,
) -> i64 {
    guard(&[stopped(ls), stopped(client)], -1, || {
        let Some(ls) = ls.as_ref() else { return -1 };
        if len > MAX_INPUT {
            set_error(
                ErrorCode::InvalidArgument,
                format_args!("input of {len} bytes: at most {MAX_INPUT} a tick"),
            );
            return -1;
        }
        let input = if data.is_null() || len == 0 {
            Bytes::new()
        } else {
            Bytes::copy_from_slice(std::slice::from_raw_parts(data, len))
        };
        let Some((tick, frame)) = ls.inner.lock().send_input(input) else {
            return -1;
        };
        if send_frame(client, frame) != 0 {
            return -1;
        }
        tick as i64
    })
}

/// Reliable command outside the input stream (buy, surrender, pause): delivered to everyone in the
/// next tick's inputs under actor 65535 as `[u16 actor, u16 len, bytes]*`, journaled and replayed.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_lockstep_send_command(
    client: *mut AurixNetClient,
    data: *const u8,
    len: usize,
) -> i32 {
    guard(&[stopped(client)], -1, || {
        let frame = (!data.is_null())
            .then(|| LockstepClient::command_frame(std::slice::from_raw_parts(data, len)))
            .flatten();
        let Some(frame) = frame else {
            set_error(
                ErrorCode::InvalidArgument,
                format_args!("send_command: 1..={MAX_COMMAND} bytes"),
            );
            return -1;
        };
        send_frame(client, frame)
    })
}
#[no_mangle]
pub unsafe extern "C" fn aurix_net_lockstep_report_hash(
    client: *mut AurixNetClient,
    tick: u32,
    hash: u64,
) -> i32 {
    guard(&[stopped(client)], -1, || {
        send_frame(client, LockstepClient::hash_frame(tick, hash))
    })
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_lockstep_send_checkpoint(
    client: *mut AurixNetClient,
    tick: u32,
    hash: u64,
    state: *const u8,
    len: usize,
) -> i32 {
    guard(&[stopped(client)], -1, || {
        let st = if state.is_null() {
            &[][..]
        } else {
            std::slice::from_raw_parts(state, len)
        };
        send_frame(client, LockstepClient::checkpoint_frame(tick, hash, st))
    })
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_lockstep_send_dump(
    client: *mut AurixNetClient,
    tick: u32,
    hash: u64,
    state: *const u8,
    len: usize,
) -> i32 {
    guard(&[stopped(client)], -1, || {
        let st = if state.is_null() {
            &[][..]
        } else {
            std::slice::from_raw_parts(state, len)
        };
        send_frame(client, LockstepClient::dump_frame(tick, hash, st))
    })
}

pub const AURIX_NET_LS_INPUT_DELAY: i32 = 0;
pub const AURIX_NET_LS_INPUT_DELAY_MIN: i32 = 1;
pub const AURIX_NET_LS_INPUT_DELAY_MAX: i32 = 2;
pub const AURIX_NET_LS_ADAPTIVE_DELAY: i32 = 3;
pub const AURIX_NET_LS_INPUT_REDUNDANCY: i32 = 4;
pub const AURIX_NET_LS_LAST_MARGIN: i32 = 5;
pub const AURIX_NET_LS_LATE_INPUTS: i32 = 6;
pub const AURIX_NET_LS_CONFIRMED_TICK: i32 = 7;
pub const AURIX_NET_LS_SERVER_TICK: i32 = 8;
pub const AURIX_NET_LS_DELIVERED: i32 = 9;
pub const AURIX_NET_LS_CATCHUPS: i32 = 10;
pub const AURIX_NET_LS_DROPPED: i32 = 11;

/// Reads a tuning knob or counter (AURIX_NET_LS_* keys); i64::MIN for an unknown key.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_lockstep_get(ls: *mut AurixNetLockstep, key: i32) -> i64 {
    guard(&[stopped(ls)], i64::MIN, || {
        let Some(ls) = ls.as_ref() else {
            return i64::MIN;
        };
        let lockstep = ls.inner.lock();
        match key {
            AURIX_NET_LS_INPUT_DELAY => lockstep.input_delay as i64,
            AURIX_NET_LS_INPUT_DELAY_MIN => lockstep.input_delay_min as i64,
            AURIX_NET_LS_INPUT_DELAY_MAX => lockstep.input_delay_max as i64,
            AURIX_NET_LS_ADAPTIVE_DELAY => lockstep.adaptive_delay as i64,
            AURIX_NET_LS_INPUT_REDUNDANCY => lockstep.input_redundancy as i64,
            AURIX_NET_LS_LAST_MARGIN => lockstep.last_margin as i64,
            AURIX_NET_LS_LATE_INPUTS => lockstep.late_inputs as i64,
            AURIX_NET_LS_CONFIRMED_TICK => lockstep.confirmed_tick() as i64,
            AURIX_NET_LS_SERVER_TICK => lockstep.server_tick as i64,
            AURIX_NET_LS_DELIVERED => lockstep.delivered as i64,
            AURIX_NET_LS_CATCHUPS => lockstep.catchups as i64,
            AURIX_NET_LS_DROPPED => lockstep.dropped as i64,
            _ => i64::MIN,
        }
    })
}

/// Sets a tuning knob (delay, min, max, adaptive, redundancy); -1 for a read-only or unknown key.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_lockstep_set(
    ls: *mut AurixNetLockstep,
    key: i32,
    value: i64,
) -> i32 {
    guard(&[stopped(ls)], -1, || {
        let Some(ls) = ls.as_ref() else { return -1 };
        let mut lockstep = ls.inner.lock();
        let clamped = value.clamp(0, u32::MAX as i64) as u32;
        match key {
            AURIX_NET_LS_INPUT_DELAY => lockstep.input_delay = clamped,
            AURIX_NET_LS_INPUT_DELAY_MIN => lockstep.input_delay_min = clamped,
            AURIX_NET_LS_INPUT_DELAY_MAX => lockstep.input_delay_max = clamped,
            AURIX_NET_LS_ADAPTIVE_DELAY => lockstep.adaptive_delay = value != 0,
            AURIX_NET_LS_INPUT_REDUNDANCY => {
                lockstep.input_redundancy = clamped.clamp(1, 8) as usize
            }
            _ => return -1,
        }
        0
    })
}

/// FNV-1a 64 over bytes: the portable state hash the SDKs share.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_lockstep_fnv1a(data: *const u8, len: usize) -> u64 {
    guard(&[], 0, || {
        if data.is_null() || len == 0 {
            return LockstepClient::fnv1a(&[]);
        }
        LockstepClient::fnv1a(std::slice::from_raw_parts(data, len))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn last_error() -> (i32, String) {
        let text = unsafe { CStr::from_ptr(aurix_net_last_error()) };
        (
            aurix_net_last_error_code(),
            text.to_str().unwrap().to_owned(),
        )
    }

    #[test]
    fn an_unknown_field_is_refused_with_the_code_of_the_node_and_named() {
        assert!(control_message(r#"{"t":"join","name":"lobby","mode":"relay"}"#).is_none());
        let (code, text) = last_error();
        assert_eq!(code, ErrorCode::InvalidRequest as i32);
        assert!(
            text.starts_with("bad message: unknown field `mode`"),
            "{text}"
        );

        let target = r#"{"t":"event","code":1,"data":"","target":{"kind":"all","colour":"red"}}"#;
        assert!(control_message(target).is_none());
        let (code, text) = last_error();
        assert_eq!(code, ErrorCode::InvalidRequest as i32);
        assert!(text.contains("colour"), "{text}");
    }

    #[test]
    fn props_of_the_game_take_any_key() {
        let join = r#"{"t":"join_or_create","name":"lobby","props":{"colour":"red"},"player_props":{"hat":1}}"#;
        assert!(control_message(join).is_some());
    }

    #[test]
    fn undelivered_events_are_read_back_as_they_were_sent() {
        let event = |code, target, payload: &'static [u8]| EventFrame {
            code,
            target,
            cache: CacheOp::None,
            group: None,
            from: ActorId(0),
            payload: Bytes::from_static(payload),
        };
        let events = [
            event(2, EventTarget::Actors(vec![ActorId(7), ActorId(9)]), b"two"),
            EventFrame {
                cache: CacheOp::Add,
                group: Some(9),
                ..event(5, EventTarget::Master, b"")
            },
        ];
        let mut frames = undelivered_frames(&events);
        let ev = AurixNetEvent {
            kind: AURIX_NET_EVENT_UNDELIVERED,
            data: frames.as_mut_ptr(),
            data_len: frames.len(),
            ..AurixNetEvent::empty()
        };
        let read = |index| unsafe {
            let mut out = std::mem::zeroed::<AurixNetUndelivered>();
            let mut actors = [0u16; 4];
            let found = aurix_net_undelivered_get(&ev, index, &mut out);
            let copied = aurix_net_undelivered_actors(&ev, index, actors.as_mut_ptr(), 4);
            let payload = (found == 0).then(|| std::slice::from_raw_parts(out.data, out.len));
            (
                found,
                (out.code, out.target, out.n_actors, out.group, out.cache),
                actors[..copied].to_vec(),
                payload,
            )
        };
        assert_eq!(unsafe { aurix_net_undelivered_count(&ev) }, 2);
        assert_eq!(
            read(0),
            (0, (2, 3, 2, -1, 0), vec![7, 9], Some(&b"two"[..]))
        );
        assert_eq!(read(1), (0, (5, 2, 0, 9, 1), vec![], Some(&b""[..])));
        assert_eq!(read(2).0, -1);
        assert_eq!(
            unsafe { aurix_net_undelivered_count(&AurixNetEvent::empty()) },
            0
        );
    }

    #[test]
    fn a_panic_fails_the_call_and_stops_its_handle() {
        let (hit, spared) = (aurix_net_lockstep_new(), aurix_net_lockstep_new());
        let delay = AURIX_NET_LS_INPUT_DELAY;
        let panicked = unsafe { run(&[stopped(hit)], -1, || panic!("no {}", "luck")) };
        assert_eq!(panicked, -1);
        assert_eq!(
            last_error(),
            (ErrorCode::Panic as i32, "panic: no luck".into())
        );

        unsafe {
            aurix_net_set_last_error(0, std::ptr::null());
            assert_eq!(aurix_net_lockstep_set(hit, delay, 4), -1);
            assert_eq!(
                last_error(),
                (ErrorCode::Panic as i32, "panic: no luck".into())
            );
            assert_eq!(aurix_net_lockstep_get(hit, delay), i64::MIN);
            assert_eq!(aurix_net_lockstep_set(spared, delay, 4), 0);
            assert_eq!(aurix_net_lockstep_get(spared, delay), 4);
            aurix_net_lockstep_destroy(hit);
            aurix_net_lockstep_destroy(spared);
        }
    }

    #[test]
    fn a_stopped_client_is_closed_with_the_panic_once() {
        let client = Stopped::default();
        let mut ev = AurixNetEvent::empty();
        assert_eq!(client.poll(&mut ev), None);

        assert_eq!(run(&[Some(&client)], 0, || panic!("lost")), 0);
        assert_eq!(client.poll(&mut ev), Some(1));
        assert_eq!(ev.kind, AURIX_NET_EVENT_CLOSED);
        assert_eq!(unsafe { cstr(ev.json) }, Some("panic: lost"));
        unsafe { aurix_net_event_free(&mut ev) };
        assert_eq!(client.poll(&mut ev), Some(0));
    }
}
