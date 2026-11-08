//! Attacker-facing decoders never panic on random or mutated input.
use bytes::Bytes;
use net_common::ActorId;
use net_rooms::event::{CacheOp, EventFrame, EventTarget};
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
#[test]
fn event_frame_decode_survives_garbage() {
    let mut r = rng(7);
    for _ in 0..20_000 {
        let _ = EventFrame::decode(Bytes::from(garbage(&mut r, 64)));
    }
}
#[test]
fn event_frame_roundtrip_then_mutations() {
    let mut r = rng(9);
    for i in 0..2_000u32 {
        let f = EventFrame {
            code: i as u8,
            target: EventTarget::Others,
            cache: CacheOp::None,
            group: if i.is_multiple_of(3) {
                Some(i as u16)
            } else {
                None
            },
            from: ActorId((i % 7) as u16),
            payload: Bytes::from(garbage(&mut r, 40)),
        };
        let enc = f.encode();
        let back = EventFrame::decode(enc.clone()).expect("own encoding decodes");
        assert_eq!((back.code, back.from), (f.code, f.from));
        for _ in 0..5 {
            let _ = EventFrame::decode(Bytes::from(mutate(&mut r, &enc)));
        }
    }
}
