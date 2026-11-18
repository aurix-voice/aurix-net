//! Datagram headers, fragments, links and tickets never panic on garbage.

use bytes::Bytes;
use net_transport::fragment::Reassembler;
use net_transport::link::Link;
use net_transport::packet::Header;
use net_transport::TicketSigner;
use proptest::collection::vec;
use proptest::prelude::*;
use std::time::Instant;

fn datagram() -> impl Strategy<Value = Vec<u8>> {
    vec(any::<u8>(), 0..=96)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20_000))]

    #[test]
    fn header_and_ticket_decoders_survive_garbage(datagram in datagram()) {
        let _ = Header::decode(Bytes::from(datagram.clone()));
        let _ = TicketSigner::new(b"secret").verify(&datagram);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// One link and one reassembler take the whole burst, so state left by one datagram meets
    /// the next.
    #[test]
    fn link_and_reassembler_survive_a_garbage_burst(burst in vec(datagram(), 0..200)) {
        let now = Instant::now();
        let mut link = Link::new(now);
        let mut reassembler = Reassembler::default();
        for datagram in burst {
            let _ = reassembler.accept(Bytes::from(datagram.clone()), now);
            let _ = link.receive(Bytes::from(datagram), now);
            let _ = link.poll(now);
            while link.next_outgoing().is_some() {}
        }
    }
}
