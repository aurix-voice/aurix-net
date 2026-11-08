//! Client datagrams and lobby filters never panic the node.
use bytes::Bytes;
use net_api::lockstep::LockstepRoom;
use net_api::mmfilter::Filter;
use net_api::sync::SyncRoom;
use net_common::ActorId;
use net_rooms::protocol::ClientMsg;
fn rng(seed: u64) -> impl FnMut() -> u64 {
    let mut s = seed | 1;
    move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    }
}
fn garbage(r: &mut impl FnMut() -> u64, max: usize) -> Vec<u8> {
    let n = (r() as usize) % (max + 1);
    (0..n).map(|_| r() as u8).collect()
}
#[allow(dead_code)]
fn mutate(r: &mut impl FnMut() -> u64, v: &[u8]) -> Vec<u8> {
    let mut m = v.to_vec();
    let k = 1 + (r() % 4) as usize;
    for _ in 0..k {
        if m.is_empty() {
            break;
        }
        let i = (r() as usize) % m.len();
        match r() % 3 {
            0 => m[i] ^= 1 << (r() % 8),
            1 => {
                m.remove(i);
            }
            _ => m.insert(i, r() as u8),
        }
    }
    m
}
fn framed(r: &mut impl FnMut() -> u64, kinds: &[u8]) -> Bytes {
    let mut g = garbage(r, 80);
    if r().is_multiple_of(2) {
        let k = kinds[(r() as usize) % kinds.len()];
        if g.is_empty() {
            g.push(k)
        } else {
            g[0] = k
        }
    }
    Bytes::from(g)
}
#[test]
fn lockstep_handle_survives_garbage() {
    let mut r = rng(13);
    let mut room = LockstepRoom::new(60, 1);
    let (a, b) = (ActorId(1), ActorId(2));
    room.on_join(a);
    room.on_join_as(b, true);
    for i in 0..20_000u32 {
        let actor = if i.is_multiple_of(5) { b } else { a };
        let _ = room.handle(
            actor,
            framed(
                &mut r,
                &[0x10, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27],
            ),
        );
        if i.is_multiple_of(50) {
            let _ = room.tick();
            let _ = room.take_desyncs();
            let _ = room.take_dumps();
        }
    }
    let _ = room.snapshot();
}
#[test]
fn sync_handle_survives_garbage() {
    let mut r = rng(17);
    let mut room = SyncRoom::new(
        Some("component Transform {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n}\n"),
        60,
        1,
    )
    .unwrap();
    let a = ActorId(1);
    room.on_join(a);
    for i in 0..20_000u32 {
        let _ = room.handle(a, framed(&mut r, &[0x10, 0x11, 0x12, 0x13]));
        if i.is_multiple_of(50) {
            let _ = room.tick();
        }
    }
}
#[test]
fn filter_parse_survives_garbage() {
    let mut r = rng(19);
    let alphabet: &[u8] =
        b"map='dust' AND OR NOT rank_min<=1400 >= != ( ) , \"x\" 0123456789.abc_ IN LIKE";
    for _ in 0..20_000 {
        let n = (r() as usize) % 80;
        let t: String = (0..n)
            .map(|_| alphabet[(r() as usize) % alphabet.len()] as char)
            .collect();
        if let Ok(f) = Filter::parse(&t) {
            let _ = f.matches(&serde_json::Map::new());
        }
        let _ = Filter::parse(&String::from_utf8_lossy(&garbage(&mut r, 40)));
    }
}

#[test]
fn control_plane_json_survives_garbage() {
    let mut r = rng(31);
    let valid = [
        r#"{"t":"auth","grant":"abc"}"#,
        r#"{"t":"create","name":"arena","mode":"sync","max_players":8,"props":{"lobby":"ranked","tick_hz":30},"lobby_props":["lobby"]}"#,
        r#"{"t":"join","name":"arena","player_props":{"skin":1}}"#,
        r#"{"t":"find_friends","players":["ann","bob"]}"#,
        r#"{"t":"stats"}"#,
        r#"{"t":"set_player_props","props":{"ready":true},"expected":{"ready":false}}"#,
        r#"{"t":"ping","ts":1}"#,
    ];
    let alphabet: &[u8] = b"{}[]:,\"tauthcreatejoinpropsnamemode0123456789.-eE nulltruefalse";
    for i in 0..20_000usize {
        let v = valid[i % valid.len()].as_bytes();
        let _ = serde_json::from_slice::<ClientMsg>(&mutate(&mut r, v));
        let n = (r() as usize) % 100;
        let t: Vec<u8> = (0..n)
            .map(|_| alphabet[(r() as usize) % alphabet.len()])
            .collect();
        let _ = serde_json::from_slice::<ClientMsg>(&t);
        let _ = serde_json::from_slice::<ClientMsg>(&garbage(&mut r, 60));
    }
    let deep = format!(
        "{{\"t\":\"create\",\"props\":{{\"a\":{}1{}}}}}",
        "[".repeat(2000),
        "]".repeat(2000)
    );
    assert!(
        serde_json::from_str::<ClientMsg>(&deep).is_err(),
        "nesting beyond serde's recursion limit is rejected, not a stack overflow"
    );
    for m in valid {
        serde_json::from_str::<ClientMsg>(m).unwrap_or_else(|e| panic!("{m}: {e}"));
    }
}
