//! Client datagrams and lobby filters never panic the node.

use bytes::Bytes;
use net_api::lockstep::LockstepRoom;
use net_api::mmfilter::Filter;
use net_api::sync::SyncRoom;
use net_common::ActorId;
use net_rooms::protocol::ClientMsg;
use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;
use proptest::sample::{select, Index};

const LOCKSTEP_KINDS: &[u8] = &[0x10, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27];
const SYNC_KINDS: &[u8] = &[0x10, 0x11, 0x12, 0x13];

/// Characters a lobby filter is written with, so the text gets past the first token.
const FILTER_CHARS: &[u8] =
    b"map='dust' AND OR NOT rank_min<=1400 >= != ( ) , \"x\" 0123456789.abc_ IN LIKE";

/// Characters of a control-plane message, so the text gets past the first token.
const JSON_CHARS: &[u8] = b"{}[]:,\"tauthcreatejoinpropsnamemode0123456789.-eE nulltruefalse";

const MESSAGES: &[&str] = &[
    r#"{"t":"auth","grant":"abc"}"#,
    r#"{"t":"create","name":"arena","mode":"sync","max_players":8,"props":{"lobby":"ranked","tick_hz":30},"lobby_props":["lobby"]}"#,
    r#"{"t":"join","name":"arena","player_props":{"skin":1}}"#,
    r#"{"t":"find_friends","players":["ann","bob"]}"#,
    r#"{"t":"stats"}"#,
    r#"{"t":"set_player_props","props":{"ready":true},"expected":{"ready":false}}"#,
    r#"{"t":"ping","ts":1}"#,
];

/// Random bytes; half of the frames open with a known kind so they reach its decoder.
fn frame(kinds: &'static [u8]) -> impl Strategy<Value = Bytes> {
    (vec(any::<u8>(), 0..=80), option::of(select(kinds))).prop_map(|(mut bytes, kind)| {
        match (bytes.first_mut(), kind) {
            (Some(first), Some(kind)) => *first = kind,
            (None, Some(kind)) => bytes.push(kind),
            _ => {}
        }
        Bytes::from(bytes)
    })
}

/// A valid message after one to four single-byte edits: a bit flip, a dropped byte or an
/// inserted one.
fn mutated_message() -> impl Strategy<Value = Vec<u8>> {
    (
        select(MESSAGES),
        vec((any::<Index>(), 0u8..3, any::<u8>()), 1..=4),
    )
        .prop_map(|(message, edits)| {
            let mut bytes = message.as_bytes().to_vec();
            for (at, kind, byte) in edits {
                let i = at.index(bytes.len());
                match kind {
                    0 => bytes[i] ^= 1 << (byte % 8),
                    1 => {
                        bytes.remove(i);
                    }
                    _ => bytes.insert(i, byte),
                }
            }
            bytes
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// One room takes the whole stream, so state left by one frame meets the next.
    #[test]
    fn lockstep_handle_survives_garbage(stream in vec(frame(LOCKSTEP_KINDS), 0..400)) {
        let mut room = LockstepRoom::new(60, 1);
        let (player, spectator) = (ActorId(1), ActorId(2));
        room.on_join(player);
        room.on_join_as(spectator, true);
        for (i, frame) in stream.into_iter().enumerate() {
            let actor = if i.is_multiple_of(5) { spectator } else { player };
            let _ = room.handle(actor, frame);
            if i.is_multiple_of(50) {
                let _ = room.tick();
                let _ = room.take_desyncs();
                let _ = room.take_dumps();
            }
        }
        let _ = room.snapshot();
    }

    /// One room takes the whole stream, so state left by one frame meets the next.
    #[test]
    fn sync_handle_survives_garbage(stream in vec(frame(SYNC_KINDS), 0..400)) {
        let schema = "component Transform {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n}\n";
        let mut room = SyncRoom::new(Some(schema), 60, 1).unwrap();
        let actor = ActorId(1);
        room.on_join(actor);
        for (i, frame) in stream.into_iter().enumerate() {
            let _ = room.handle(actor, frame);
            if i.is_multiple_of(50) {
                let _ = room.tick();
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20_000))]

    #[test]
    fn filter_parse_survives_filter_like_text(text in vec(select(FILTER_CHARS), 0..80)) {
        if let Ok(filter) = Filter::parse(&String::from_utf8_lossy(&text)) {
            let _ = filter.matches(&serde_json::Map::new());
        }
    }

    #[test]
    fn filter_parse_survives_garbage_text(bytes in vec(any::<u8>(), 0..=40)) {
        let _ = Filter::parse(&String::from_utf8_lossy(&bytes));
    }

    #[test]
    fn control_plane_json_survives_a_mutated_message(bytes in mutated_message()) {
        let _ = serde_json::from_slice::<ClientMsg>(&bytes);
    }

    #[test]
    fn control_plane_json_survives_json_like_text(text in vec(select(JSON_CHARS), 0..100)) {
        let _ = serde_json::from_slice::<ClientMsg>(&text);
    }

    #[test]
    fn control_plane_json_survives_garbage(bytes in vec(any::<u8>(), 0..=60)) {
        let _ = serde_json::from_slice::<ClientMsg>(&bytes);
    }
}

#[test]
fn control_plane_json_accepts_the_valid_messages() {
    for message in MESSAGES {
        serde_json::from_str::<ClientMsg>(message).unwrap_or_else(|e| panic!("{message}: {e}"));
    }
}

#[test]
fn control_plane_json_rejects_nesting_beyond_the_recursion_limit() {
    let deep = format!(
        "{{\"t\":\"create\",\"props\":{{\"a\":{}1{}}}}}",
        "[".repeat(2000),
        "]".repeat(2000)
    );
    assert!(
        serde_json::from_str::<ClientMsg>(&deep).is_err(),
        "nesting beyond serde's recursion limit is rejected, not a stack overflow"
    );
}
