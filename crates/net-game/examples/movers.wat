;; Moving objects for bandwidth measurements: the room keeps N server-owned entities and changes
;; every one of them on every tick (pos on all three axes and yaw), the worst case for deltas.
;; N comes from the clients: an input is a u16 (little endian) object count, the largest one seen
;; wins (capped at 2048). Actors get no avatar.
;; Upload: aurix-net module-put movers crates/net-game/examples/movers.wat
;; Room props: {"schema": "component Transform {\n pos: vec3 @range(-4096, 4096, 0.01)\n yaw: f32 @range(0, 360, 0.1)\n}",
;;              "logic": "movers", "aoi_cell": 0}
(module
  (import "aurix" "spawn" (func $spawn (param i32) (result i64)))
  (import "aurix" "entity_count" (func $entity_count (result i32)))
  (import "aurix" "entity_at" (func $entity_at (param i32) (result i64)))
  (import "aurix" "component_id" (func $cid (param i32 i32) (result i32)))
  (import "aurix" "set_vec3" (func $set_vec3 (param i64 i32 i32 f32 f32 f32) (result i32)))
  (import "aurix" "set_f32" (func $set_f32 (param i64 i32 i32 f32) (result i32)))
  (import "aurix" "input_count" (func $input_count (result i32)))
  (import "aurix" "input_read" (func $input_read (param i32 i32 i32) (result i32)))
  (import "aurix" "tick" (func $tick (result i64)))
  (import "aurix" "dt" (func $dt (result f32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "Transform")
  (global $tf (mut i32) (i32.const -1))
  (global $want (mut i32) (i32.const 0))
  ;; triangle wave 0..p..0 with period 2p: moves by exactly 1 every step
  (func $tri (param $k i32) (param $p i32) (result f32)
    (local $m i32)
    (local.set $m (i32.rem_u (local.get $k) (i32.shl (local.get $p) (i32.const 1))))
    (f32.convert_i32_s (select
      (local.get $m)
      (i32.sub (i32.shl (local.get $p) (i32.const 1)) (local.get $m))
      (i32.lt_u (local.get $m) (local.get $p)))))
  (func (export "aurix_init") (param $seed i64)
    (global.set $tf (call $cid (i32.const 0) (i32.const 9))))
  (func (export "aurix_step")
    (local $i i32) (local $n i32) (local $t i32) (local $w i32) (local $e i64) (local $s f32)
    (local.set $n (call $input_count))
    (block $done (loop $l
      (br_if $done (i32.ge_s (local.get $i) (local.get $n)))
      (i32.store16 (i32.const 64) (i32.const 0))
      (drop (call $input_read (local.get $i) (i32.const 64) (i32.const 2)))
      (local.set $w (i32.load16_u (i32.const 64)))
      (if (i32.gt_u (local.get $w) (global.get $want)) (then (global.set $want (local.get $w))))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br $l)))
    (if (i32.gt_u (global.get $want) (i32.const 2048)) (then (global.set $want (i32.const 2048))))
    (local.set $n (call $entity_count))
    (block $full (loop $l
      (br_if $full (i32.ge_s (local.get $n) (global.get $want)))
      (br_if $full (i64.eqz (call $spawn (i32.const 0))))
      (local.set $n (i32.add (local.get $n) (i32.const 1)))
      (br $l)))
    (local.set $n (call $entity_count))
    (local.set $t (i32.wrap_i64 (call $tick)))
    (local.set $s (call $dt))
    (local.set $i (i32.const 0))
    (block $done (loop $l
      (br_if $done (i32.ge_s (local.get $i) (local.get $n)))
      (local.set $e (call $entity_at (local.get $i)))
      ;; 4-unit grid, 25 per row; 5 units/s on x and z, 1 unit/s on y
      (drop (call $set_vec3 (local.get $e) (global.get $tf) (i32.const 0)
        (f32.add
          (f32.convert_i32_s (i32.sub (i32.mul (i32.rem_u (local.get $i) (i32.const 25)) (i32.const 4)) (i32.const 50)))
          (f32.mul (call $tri (i32.add (local.get $t) (i32.mul (local.get $i) (i32.const 37))) (i32.const 200))
            (f32.mul (f32.const 5) (local.get $s))))
        (f32.mul (call $tri (i32.add (local.get $t) (i32.mul (local.get $i) (i32.const 11))) (i32.const 40))
          (local.get $s))
        (f32.add
          (f32.convert_i32_s (i32.sub (i32.mul (i32.div_u (local.get $i) (i32.const 25)) (i32.const 4)) (i32.const 50)))
          (f32.mul (call $tri (i32.add (i32.add (local.get $t) (i32.const 100)) (i32.mul (local.get $i) (i32.const 53))) (i32.const 200))
            (f32.mul (f32.const 5) (local.get $s))))))
      ;; 0.9 degrees per tick
      (drop (call $set_f32 (local.get $e) (global.get $tf) (i32.const 1)
        (f32.div
          (f32.convert_i32_s (i32.rem_u
            (i32.add (i32.mul (local.get $t) (i32.const 9)) (i32.mul (local.get $i) (i32.const 130)))
            (i32.const 3600)))
          (f32.const 10))))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br $l))))
)
