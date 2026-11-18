//! Attacker-facing decoders never panic on random or mutated input.

use bytes::Bytes;
use net_common::ActorId;
use net_rooms::event::{CacheOp, EventFrame, EventTarget};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::Index;

fn event_frame() -> impl Strategy<Value = EventFrame> {
    let target = prop_oneof![
        Just(EventTarget::All),
        Just(EventTarget::Others),
        Just(EventTarget::Master),
        vec(any::<u16>().prop_map(ActorId), 0..8).prop_map(EventTarget::Actors),
    ];
    let cache = prop_oneof![
        Just(CacheOp::None),
        Just(CacheOp::Add),
        Just(CacheOp::Remove)
    ];
    (
        any::<u8>(),
        target,
        cache,
        any::<Option<u16>>(),
        any::<u16>(),
        vec(any::<u8>(), 0..=40),
    )
        .prop_map(|(code, target, cache, group, from, payload)| EventFrame {
            code,
            target,
            cache,
            group,
            from: ActorId(from),
            payload: Bytes::from(payload),
        })
}

/// One to four single-byte edits of an encoding: a bit flip, a dropped byte or an inserted one.
fn edits() -> impl Strategy<Value = Vec<(Index, u8, u8)>> {
    vec((any::<Index>(), 0u8..3, any::<u8>()), 1..=4)
}

fn apply(edits: &[(Index, u8, u8)], encoded: &[u8]) -> Vec<u8> {
    let mut bytes = encoded.to_vec();
    for (at, kind, byte) in edits {
        if bytes.is_empty() {
            break;
        }
        let i = at.index(bytes.len());
        match kind {
            0 => bytes[i] ^= 1 << (byte % 8),
            1 => {
                bytes.remove(i);
            }
            _ => bytes.insert(i, *byte),
        }
    }
    bytes
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20_000))]

    #[test]
    fn event_frame_decode_survives_garbage(bytes in vec(any::<u8>(), 0..=64)) {
        let _ = EventFrame::decode(Bytes::from(bytes));
    }

    #[test]
    fn event_frame_roundtrips(frame in event_frame()) {
        prop_assert_eq!(EventFrame::decode(frame.encode()), Ok(frame));
    }

    #[test]
    fn event_frame_decode_survives_a_mutated_encoding(frame in event_frame(), edits in edits()) {
        let _ = EventFrame::decode(Bytes::from(apply(&edits, &frame.encode())));
    }
}
