//! Event frames and control messages from hostile clients must be rejected, never panic.
use bytes::Bytes;
use net_rooms::protocol::ClientMsg;
use net_rooms::EventFrame;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn event_frames_from_garbage() {
    let mut rng = Rng(7);
    for _ in 0..50_000 {
        let n = (rng.next() % 64) as usize;
        let b: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        let _ = EventFrame::decode(Bytes::from(b));
    }
}

#[test]
fn control_messages_from_garbage() {
    let mut rng = Rng(11);
    let seeds = [
        r#"{"t":"join","name":"x"}"#,
        r#"{"t":"create","mode":"sync","props":{"tick_hz":99999999999,"schema":"component X { a: vec3 @range(1,0,0) }"}}"#,
        r#"{"t":"event","code":300,"data":"!!!","target":{"kind":"actors","actors":[1,2]}}"#,
        r#"{"t":"set_room_props","props":{"a":{"b":{"c":[[[[[[[[[[1]]]]]]]]]]}}}}"#,
        r#"{"t":"auth","grant":""}"#,
        r#"{"t":"kick","actor":65535}"#,
    ];
    for s in seeds {
        let _ = serde_json::from_str::<ClientMsg>(s);
        let bytes = s.as_bytes();
        for _ in 0..2_000 {
            let mut v = bytes.to_vec();
            for _ in 0..1 + rng.next() % 3 {
                let i = (rng.next() as usize) % v.len();
                v[i] = rng.next() as u8;
            }
            if let Ok(txt) = std::str::from_utf8(&v) {
                let _ = serde_json::from_str::<ClientMsg>(txt);
            }
        }
    }
    // deep nesting must be refused, not overflow the stack
    let deep = format!(
        "{{\"t\":\"set_room_props\",\"props\":{{\"a\":{}1{}}}}}",
        "[".repeat(100_000),
        "]".repeat(100_000)
    );
    assert!(serde_json::from_str::<ClientMsg>(&deep).is_err());
}
