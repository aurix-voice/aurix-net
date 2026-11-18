//! Delta application never panics on random input; the frame stays usable.

use net_replication::Replicator;
use net_sim::{Frame, FrameConfig};
use net_wire::schema::Schema;
use proptest::collection::vec;
use proptest::prelude::*;

const SCHEMA: &str =
    "component T {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  hp: u8\n  name: string(8)\n}\n";

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// One frame takes the whole stream, so state left by one delta meets the next.
    #[test]
    fn apply_survives_garbage(stream in vec(vec(any::<u8>(), 0..=96), 0..200)) {
        let schema = Schema::parse(SCHEMA).unwrap();
        let mut frame = Frame::new(schema, FrameConfig::default(), 1);
        for delta in stream {
            let _ = Replicator::apply(&mut frame, &delta);
        }
        let _ = frame.snapshot();
    }
}
