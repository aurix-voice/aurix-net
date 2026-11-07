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
    println!("physics native {native:#x}\nphysics wasm   {wasm:#x}");
    assert_eq!(native, wasm, "wasm32 and native simulations diverged");
    let j_native = net_sim_wasm::sim_joint_golden_hash();
    let jf = instance
        .get_typed_func::<(), u64>(&mut store, "sim_joint_golden_hash")
        .expect("export");
    let j_wasm = jf.call(&mut store, ()).expect("call");
    println!("joints native {j_native:#x}\njoints wasm   {j_wasm:#x}");
    assert_eq!(
        j_native, j_wasm,
        "wasm32 and native joint simulations diverged"
    );
    let d3_native = net_sim_wasm::sim3_golden_hash();
    let d3f = instance
        .get_typed_func::<(), u64>(&mut store, "sim3_golden_hash")
        .expect("export");
    let d3_wasm = d3f.call(&mut store, ()).expect("call");
    println!("3d native {d3_native:#x}\n3d wasm   {d3_wasm:#x}");
    assert_eq!(
        d3_native, d3_wasm,
        "wasm32 and native 3D simulations diverged"
    );
    let j3_native = net_sim_wasm::sim3_joint_golden_hash();
    let j3f = instance
        .get_typed_func::<(), u64>(&mut store, "sim3_joint_golden_hash")
        .expect("export");
    let j3_wasm = j3f.call(&mut store, ()).expect("call");
    println!("3d joints native {j3_native:#x}\n3d joints wasm   {j3_wasm:#x}");
    assert_eq!(
        j3_native, j3_wasm,
        "wasm32 and native 3D joint simulations diverged"
    );
    let h3_native = net_sim_wasm::sim3_hull_golden_hash();
    let h3f = instance
        .get_typed_func::<(), u64>(&mut store, "sim3_hull_golden_hash")
        .expect("export");
    let h3_wasm = h3f.call(&mut store, ()).expect("call");
    println!("3d hulls native {h3_native:#x}\n3d hulls wasm   {h3_wasm:#x}");
    assert_eq!(
        h3_native, h3_wasm,
        "wasm32 and native convex hull simulations diverged"
    );
    let nav_native = net_sim_wasm::sim_nav_golden_hash();
    let g = instance
        .get_typed_func::<(), u64>(&mut store, "sim_nav_golden_hash")
        .expect("export");
    let nav_wasm = g.call(&mut store, ()).expect("call");
    println!("nav native {nav_native:#x}\nnav wasm   {nav_wasm:#x}");
    assert_eq!(
        nav_native, nav_wasm,
        "wasm32 and native navigation diverged"
    );
    println!("OK: identical");
}
