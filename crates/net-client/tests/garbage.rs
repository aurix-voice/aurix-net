//! The lockstep client never panics on garbage from the node.
use net_client::lockstep::LockstepClient;
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
fn lockstep_client_receive_survives_garbage() {
    let mut r = rng(23);
    let mut c = LockstepClient::new();
    for _ in 0..20_000 {
        let mut g = garbage(&mut r, 96);
        if r().is_multiple_of(2) && !g.is_empty() {
            g[0] = [0x20u8, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27][(r() as usize) % 8];
        }
        let _ = c.receive(&g);
    }
}
