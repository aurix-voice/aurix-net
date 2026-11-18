//! The lockstep client never panics on garbage from the node.

use net_client::lockstep::LockstepClient;
use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;
use proptest::sample::select;

/// Message kinds the lockstep client reads.
const KINDS: &[u8] = &[0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27];

/// Random bytes; half of the frames open with a known kind so they reach its decoder.
fn frame() -> impl Strategy<Value = Vec<u8>> {
    (vec(any::<u8>(), 0..=96), option::of(select(KINDS))).prop_map(|(mut bytes, kind)| {
        if let (Some(first), Some(kind)) = (bytes.first_mut(), kind) {
            *first = kind;
        }
        bytes
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// One client takes the whole stream, so state left by one frame meets the next.
    #[test]
    fn lockstep_client_receive_survives_garbage(stream in vec(frame(), 0..200)) {
        let mut client = LockstepClient::new();
        for frame in stream {
            let _ = client.receive(&frame);
        }
    }
}
