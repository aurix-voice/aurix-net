//! Schema text and component codecs never panic on random input.

use net_wire::bits::BitReader;
use net_wire::schema::Schema;
use net_wire::value::ComponentValue;
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::select;

const SCHEMA: &str = "component T {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n  name: string(16)\n  hp: u8\n  flags: bits(5)\n  blob: bytes(8)\n  f: f32\n  i: i32\n  b: bool\n  v2: vec2 @range(-10, 10, 0.1)\n}\n";

/// Characters a schema is written with, so the text gets past the first token.
const SCHEMA_CHARS: &[u8] = b"component T{}:@range(),-.0123456789 \n\tabcxyz_vec3quatstringbitsbytesf32u8i16bool@owner_only@global";

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20_000))]

    #[test]
    fn schema_parse_survives_schema_like_text(text in vec(select(SCHEMA_CHARS), 0..120)) {
        let _ = Schema::parse(&String::from_utf8_lossy(&text));
    }

    #[test]
    fn schema_parse_survives_garbage_text(bytes in vec(any::<u8>(), 0..=60)) {
        let _ = Schema::parse(&String::from_utf8_lossy(&bytes));
    }

    #[test]
    fn component_codecs_survive_garbage_bits(bits in vec(any::<u8>(), 0..=48)) {
        let schema = Schema::parse(SCHEMA).unwrap();
        let component = schema.component("T").unwrap();
        let base = ComponentValue::zero(component);
        let _ = ComponentValue::decode_full(component, &mut BitReader::new(&bits));
        let _ = ComponentValue::decode_delta(&base, component, &mut BitReader::new(&bits));
    }
}
