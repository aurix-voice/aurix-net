//! Runs `sim_golden_hash` from the wasm32 build under Wasmtime and compares it with the native
//! value: `cargo build --release --target wasm32-unknown-unknown -p net-sim-wasm && cargo run -p net-sim-wasm --example golden_check`.
fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/wasm32-unknown-unknown/release/net_sim_wasm.wasm".into());
    let native = net_sim_wasm::sim_golden_hash();
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::from_file(&engine, &path).expect("load wasm");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("instantiate");
    let f = instance
        .get_typed_func::<(), u64>(&mut store, "sim_golden_hash")
        .expect("export");
    let wasm = f.call(&mut store, ()).expect("call");
    println!("native {native:#x}\nwasm   {wasm:#x}");
    assert_eq!(native, wasm, "wasm32 and native simulations diverged");
    println!("OK: identical");
}
