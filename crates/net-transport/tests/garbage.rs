//! Datagram headers, fragments, links and tickets never panic on garbage.
use bytes::Bytes;
use net_transport::fragment::Reassembler;
use net_transport::link::Link;
use net_transport::packet::Header;
use net_transport::TicketSigner;
use std::time::Instant;
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
fn transport_decoders_survive_garbage() {
    let mut r = rng(29);
    let now = Instant::now();
    let mut link = Link::new(now);
    let mut re = Reassembler::default();
    let signer = TicketSigner::new(b"secret");
    for _ in 0..20_000 {
        let g = garbage(&mut r, 96);
        let _ = Header::decode(Bytes::from(g.clone()));
        let _ = re.accept(Bytes::from(g.clone()), now);
        let _ = link.receive(Bytes::from(g.clone()), now);
        let _ = signer.verify(&g);
        let _ = link.poll(now);
        while link.next_outgoing().is_some() {}
    }
}
