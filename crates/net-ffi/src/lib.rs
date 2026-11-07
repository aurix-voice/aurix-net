//! C ABI over `net-client`. One global tokio runtime; every call is synchronous and safe to
//! make from a game thread. Events are pulled with `aurix_net_poll` (non-blocking).
//! Strings are UTF-8, NUL-terminated. Header: `include/aurix_net.h`.
#![allow(clippy::missing_safety_doc)]

use bytes::Bytes;
use net_client::protocol::{ClientMsg, ServerMsg};
use net_client::{Client, Event, EventTarget, Options};
use net_common::{ActorId, Channel};
use parking_lot::Mutex;
use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::sync::OnceLock;

fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("aurix-net")
            .build()
            .expect("tokio runtime")
    })
}

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_error(e: impl std::fmt::Display) {
    let s = CString::new(e.to_string().replace('\0', " ")).unwrap_or_default();
    LAST_ERROR.with(|l| *l.borrow_mut() = s);
}

unsafe fn cstr<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok()
}

pub struct AurixNetClient {
    inner: Mutex<Client>,
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
    static V: &[u8] = b"0.1.0\0";
    V.as_ptr() as *const c_char
}

/// Last error message for the calling thread (valid until the next failing call).
#[no_mangle]
pub extern "C" fn aurix_net_last_error() -> *const c_char {
    LAST_ERROR.with(|l| l.borrow().as_ptr())
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
    let (Some(http), Some(key), Some(pid), Some(name)) =
        (cstr(http), cstr(api_key), cstr(player_id), cstr(name))
    else {
        set_error("null or non-utf8 argument");
        return std::ptr::null_mut();
    };
    match rt().block_on(net_client::issue_grant(http, key, pid, name)) {
        Ok(g) => CString::new(g)
            .map(CString::into_raw)
            .unwrap_or(std::ptr::null_mut()),
        Err(e) => {
            set_error(e);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

/// Opens the control channel and authenticates. `tunnel != 0` forces the WebSocket tunnel.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_connect(
    http: *const c_char,
    grant: *const c_char,
    tunnel: i32,
) -> *mut AurixNetClient {
    let (Some(http), Some(grant)) = (cstr(http), cstr(grant)) else {
        set_error("null or non-utf8 argument");
        return std::ptr::null_mut();
    };
    let opts = Options {
        tunnel: tunnel != 0,
        ..Default::default()
    };
    match rt().block_on(Client::connect(http, grant, opts)) {
        Ok(c) => Box::into_raw(Box::new(AurixNetClient {
            inner: Mutex::new(c),
        })),
        Err(e) => {
            set_error(e);
            std::ptr::null_mut()
        }
    }
}

/// Sends a control message given as protocol JSON (`{"t":"set_room_props",...}`). 0 on success.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_send_control(
    h: *mut AurixNetClient,
    json: *const c_char,
) -> i32 {
    let (Some(h), Some(json)) = (h.as_ref(), cstr(json)) else {
        return -1;
    };
    let msg: ClientMsg = match serde_json::from_str(json) {
        Ok(m) => m,
        Err(e) => {
            set_error(e);
            return -1;
        }
    };
    match rt().block_on(h.inner.lock().send(msg)) {
        Ok(()) => 0,
        Err(e) => {
            set_error(e);
            -1
        }
    }
}

/// Joins/creates a room with a protocol JSON message (`{"t":"join_or_create","name":...}`),
/// opens the data path and blocks until it is ready. Returns the actor id, or -1.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_join(h: *mut AurixNetClient, json: *const c_char) -> i32 {
    let (Some(h), Some(json)) = (h.as_ref(), cstr(json)) else {
        return -1;
    };
    let msg: ClientMsg = match serde_json::from_str(json) {
        Ok(m) => m,
        Err(e) => {
            set_error(e);
            return -1;
        }
    };
    let mut c = h.inner.lock();
    match rt().block_on(c.join(msg)) {
        Ok(j) => j.actor.0 as i32,
        Err(e) => {
            set_error(e);
            -1
        }
    }
}

/// Room facts after a successful join as JSON (`{"room","name","mode","actor","master","tick_hz","schema_hash"}`),
/// malloc'd; NULL when not in a room.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_joined_json(h: *mut AurixNetClient) -> *mut c_char {
    let Some(h) = h.as_ref() else {
        return std::ptr::null_mut();
    };
    let c = h.inner.lock();
    let Some(j) = &c.joined else {
        return std::ptr::null_mut();
    };
    let v = serde_json::json!({
        "room": j.room, "name": j.name, "mode": j.mode, "actor": j.actor.0, "master": j.master.0,
        "tick_hz": j.tick_hz, "schema_hash": j.schema_hash,
    });
    CString::new(v.to_string())
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

/// Raw data-path message. channel: 0 unreliable-sequenced, 1 reliable-ordered, 2 reliable-unordered.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_send_data(
    h: *mut AurixNetClient,
    channel: u8,
    data: *const u8,
    len: usize,
) -> i32 {
    let Some(h) = h.as_ref() else { return -1 };
    let Some(ch) = Channel::from_u8(channel) else {
        set_error("bad channel");
        return -1;
    };
    let payload = if data.is_null() || len == 0 {
        Bytes::new()
    } else {
        Bytes::copy_from_slice(std::slice::from_raw_parts(data, len))
    };
    match h.inner.lock().send_data(ch, payload) {
        Ok(()) => 0,
        Err(e) => {
            set_error(e);
            -1
        }
    }
}

/// Relay event. target: 0 all, 1 others, 2 master, 3 explicit `actors[n]`.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_send_event(
    h: *mut AurixNetClient,
    code: u8,
    target: i32,
    actors: *const u16,
    n_actors: usize,
    data: *const u8,
    len: usize,
    reliable: i32,
) -> i32 {
    let Some(h) = h.as_ref() else { return -1 };
    let target = match target {
        0 => EventTarget::All,
        1 => EventTarget::Others,
        2 => EventTarget::Master,
        3 if !actors.is_null() => EventTarget::Actors(
            std::slice::from_raw_parts(actors, n_actors)
                .iter()
                .map(|a| ActorId(*a))
                .collect(),
        ),
        _ => {
            set_error("bad target");
            return -1;
        }
    };
    let payload = if data.is_null() || len == 0 {
        Bytes::new()
    } else {
        Bytes::copy_from_slice(std::slice::from_raw_parts(data, len))
    };
    match h
        .inner
        .lock()
        .send_event(code, target, payload, reliable != 0)
    {
        Ok(()) => 0,
        Err(e) => {
            set_error(e);
            -1
        }
    }
}

/// Pushes queued unreliable messages onto the wire; call once per frame after sending.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_flush(h: *mut AurixNetClient) {
    if let Some(h) = h.as_ref() {
        h.inner.lock().flush();
    }
}

/// Non-blocking: fills `out` and returns 1 when an event was available, 0 otherwise.
/// Free the event with `aurix_net_event_free`.
#[no_mangle]
pub unsafe extern "C" fn aurix_net_poll(h: *mut AurixNetClient, out: *mut AurixNetEvent) -> i32 {
    let (Some(h), Some(out)) = (h.as_ref(), out.as_mut()) else {
        return 0;
    };
    *out = AurixNetEvent::empty();
    let ev = h.inner.lock().try_next_event();
    let Some(ev) = ev else { return 0 };
    match ev {
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
            let mut b = payload.to_vec().into_boxed_slice();
            out.data_len = b.len();
            out.data = b.as_mut_ptr();
            std::mem::forget(b);
        }
        Event::DataReady => out.kind = AURIX_NET_EVENT_DATA_READY,
        Event::Closed(r) => {
            out.kind = AURIX_NET_EVENT_CLOSED;
            out.json = CString::new(r)
                .map(CString::into_raw)
                .unwrap_or(std::ptr::null_mut());
        }
    }
    1
}

fn json_c(msg: &ServerMsg) -> *mut c_char {
    serde_json::to_string(msg)
        .ok()
        .and_then(|s| CString::new(s).ok())
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_event_free(ev: *mut AurixNetEvent) {
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
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_leave(h: *mut AurixNetClient) -> i32 {
    let Some(h) = h.as_ref() else { return -1 };
    let mut c = h.inner.lock();
    match rt().block_on(c.leave()) {
        Ok(()) => 0,
        Err(e) => {
            set_error(e);
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn aurix_net_destroy(h: *mut AurixNetClient) {
    if !h.is_null() {
        drop(Box::from_raw(h));
    }
}
