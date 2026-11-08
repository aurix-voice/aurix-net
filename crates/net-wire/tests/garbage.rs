//! Schema text and component codecs never panic on random input.
use net_wire::bits::BitReader;
use net_wire::schema::Schema;
use net_wire::value::ComponentValue;
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
const SCHEMA: &str = "component T {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n  name: string(16)\n  hp: u8\n  flags: bits(5)\n  blob: bytes(8)\n  f: f32\n  i: i32\n  b: bool\n  v2: vec2 @range(-10, 10, 0.1)\n}\n";
#[test]
fn schema_parse_survives_garbage_text() {
    let mut r = rng(3);
    let alphabet: &[u8] = b"component T{}:@range(),-.0123456789 \n\tabcxyz_vec3quatstringbitsbytesf32u8i16bool@owner_only@global";
    for _ in 0..20_000 {
        let n = (r() as usize) % 120;
        let t: String = (0..n)
            .map(|_| alphabet[(r() as usize) % alphabet.len()] as char)
            .collect();
        let _ = Schema::parse(&t);
        let _ = Schema::parse(&String::from_utf8_lossy(&garbage(&mut r, 60)));
    }
}
#[test]
fn component_codecs_survive_garbage_bits() {
    let s = Schema::parse(SCHEMA).unwrap();
    let c = s.component("T").unwrap();
    let base = ComponentValue::zero(c);
    let mut r = rng(5);
    for _ in 0..20_000 {
        let g = garbage(&mut r, 48);
        let _ = ComponentValue::decode_full(c, &mut BitReader::new(&g));
        let _ = ComponentValue::decode_delta(&base, c, &mut BitReader::new(&g));
    }
}
