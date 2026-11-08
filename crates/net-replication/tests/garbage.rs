//! Delta application never panics on random input; the frame stays usable.
use net_replication::Replicator;
use net_sim::{Frame, FrameConfig};
use net_wire::schema::Schema;
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
fn apply_survives_garbage() {
    let s = Schema::parse(
        "component T {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  hp: u8\n  name: string(8)\n}\n",
    )
    .unwrap();
    let mut f = Frame::new(s, FrameConfig::default(), 1);
    let mut r = rng(11);
    for _ in 0..20_000 {
        let _ = Replicator::apply(&mut f, &garbage(&mut r, 96));
    }
    let _ = f.snapshot();
}
