//! IDL text and bit-packed values from untrusted sources.
use net_wire::{BitReader, ComponentValue, Schema};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn schema_parser_from_garbage() {
    let mut rng = Rng(5);
    let base = "component Transform {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n  hp: u8\n  name: string(24)\n  f: bits(5)\n  s: f32 @range(0, 1, 0.5)\n  b: bytes(3)\n}\ncomponent Tag @owner_only {\n  x: i64\n}\n";
    assert!(Schema::parse(base).is_ok());
    let alphabet: Vec<char> = "component{}():@,.-0123456789 abcdefghijklmnopqrstuvwxyz\n_\t"
        .chars()
        .collect();
    for _ in 0..3_000 {
        let mut s: Vec<char> = base.chars().collect();
        for _ in 0..1 + rng.next() % 6 {
            let i = (rng.next() as usize) % s.len();
            s[i] = alphabet[(rng.next() as usize) % alphabet.len()];
        }
        let txt: String = s.into_iter().collect();
        let _ = Schema::parse(&txt);
    }
    // absurd sizes are clamped or rejected, never allocate blindly
    for t in [
        "string(4294967295)",
        "bytes(99999999999999)",
        "bits(0)",
        "bits(65)",
        "vec3 @range(0,0,0)",
        "f32 @range(-1e38,1e38,1e-38)",
    ] {
        let _ = Schema::parse(&format!("component A {{\n  x: {t}\n}}\n"));
    }
    let many = (0..70_000)
        .map(|i| format!("component C{i} {{\n  a: u8\n}}\n"))
        .collect::<String>();
    let _ = Schema::parse(&many);
}

#[test]
fn values_from_garbage_bits() {
    let mut rng = Rng(9);
    let schema = Schema::parse("component T {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n  hp: u8\n  name: string(24)\n  b: bytes(1000)\n  f: f32\n  i: i64\n}\n").unwrap();
    let c = &schema.components[0];
    let base = ComponentValue::zero(c);
    for _ in 0..20_000 {
        let n = (rng.next() % 80) as usize;
        let buf: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        let _ = ComponentValue::decode_full(c, &mut BitReader::new(&buf));
        let _ = ComponentValue::decode_delta(&base, c, &mut BitReader::new(&buf));
    }
}
