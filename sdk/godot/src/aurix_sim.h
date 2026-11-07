// aurix-sim C ABI: deterministic fixed-point (Q32.32, raw int64 values) physics 2D/3D, navmesh,
// flow fields, crowd and reference golden hashes. Exported by libaurix_net (crates/net-ffi) and by
// libnet_sim_wasm / net_sim_wasm.wasm (crates/net-sim-wasm). Generated from the Rust sources by
// tools/gen_sim_header.py - edit there. World handles are 1-based (0 = failure); body/joint/agent
// ids are returned as 1-based too where the Rust doc says so; positions and velocities are raw Fx
// (value * 2^32).
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define AURIX_FX_ONE ((int64_t)1 << 32)
static inline int64_t aurix_fx_from_double(double v) { return (int64_t)(v * 4294967296.0); }
static inline double aurix_fx_to_double(int64_t v) { return (double)v / 4294967296.0; }

/* Create a world: gravity (raw Fx), tick rate; returns a handle. */
uint32_t sim_world_new(int64_t gravity_x, int64_t gravity_y, uint32_t tick_hz);

void sim_world_free(uint32_t h);

void sim_step(uint32_t h);

uint32_t sim_tick(uint32_t h);

uint64_t sim_hash(uint32_t h);

/* Snapshot into an internal buffer; returns its length (read with `sim_snapshot_byte`). */
uint32_t sim_snapshot(uint32_t h);

uint32_t sim_snapshot_byte(uint32_t h, uint32_t i);

/* Restore from the internal buffer (fill it with `sim_snapshot_set_byte` first). */
void sim_snapshot_set_len(uint32_t h, uint32_t len);

void sim_snapshot_set_byte(uint32_t h, uint32_t i, uint32_t v);

uint32_t sim_restore(uint32_t h);

uint32_t sim_add_circle(uint32_t h, uint32_t k, int64_t x, int64_t y, int64_t radius, int64_t density);

uint32_t sim_add_box(uint32_t h, uint32_t k, int64_t x, int64_t y, int64_t half_w, int64_t half_h, int64_t density);

uint32_t sim_add_capsule(uint32_t h, uint32_t k, int64_t x, int64_t y, int64_t half_len, int64_t radius, int64_t density);

void sim_remove_body(uint32_t h, uint32_t id);

void sim_set_material(uint32_t h, uint32_t id, int64_t friction, int64_t restitution);

void sim_set_layers(uint32_t h, uint32_t id, uint32_t layer, uint32_t mask, uint32_t sensor);

void sim_set_velocity(uint32_t h, uint32_t id, int64_t vx, int64_t vy, int64_t w);

void sim_set_position(uint32_t h, uint32_t id, int64_t x, int64_t y, int64_t angle);

void sim_apply_impulse(uint32_t h, uint32_t id, int64_t ix, int64_t iy);

/* Body state accessors: field 0..=4 = x, y, angle, vx, vy, 5 = angular velocity. */
int64_t sim_body_get(uint32_t h, uint32_t id, uint32_t field);

/* Raycast; returns body id (0 = miss) and stores fraction/point/normal for `sim_ray_get` (0..=4). */
uint32_t sim_raycast(uint32_t h, int64_t x, int64_t y, int64_t dx, int64_t dy, uint32_t mask);

int64_t sim_ray_get(uint32_t field);

/* Character controller over a kinematic body; returns a controller index. */
uint32_t sim_kcc_new(uint32_t h, uint32_t body, int64_t step_height, int64_t min_ground_ny);

/* Move the controller by (dx, dy); returns 1 when grounded afterwards. */
uint32_t sim_kcc_move(uint32_t h, uint32_t k, int64_t dx, int64_t dy);

/* Reference scenario used to prove cross-target determinism: a box pyramid plus bouncing balls simulated for 240 ticks. Native tests and the wasm build must return the same value. */
uint64_t sim_golden_hash(void);

/* Distance/rope joint between local anchors; `min_len < len` makes it a rope (slack allowed). */
uint32_t sim_joint_distance(uint32_t h, uint32_t a, uint32_t b, int64_t ax, int64_t ay, int64_t bx, int64_t by, int64_t len, int64_t min_len);

/* Revolute (pin) joint; `has_limits` != 0 enables the [lower, upper] angle range; the motor is active when `max_torque` > 0. */
uint32_t sim_joint_revolute(uint32_t h, uint32_t a, uint32_t b, int64_t ax, int64_t ay, int64_t bx, int64_t by, uint32_t has_limits, int64_t lower, int64_t upper, int64_t motor_speed, int64_t max_torque);

uint32_t sim_joint_weld(uint32_t h, uint32_t a, uint32_t b, int64_t ax, int64_t ay, int64_t bx, int64_t by, int64_t rel_angle);

uint32_t sim_joint_remove(uint32_t h, uint32_t id);

/* Retune a revolute motor at runtime (no-op for other joint kinds). */
void sim_joint_motor(uint32_t h, uint32_t id, int64_t speed, int64_t max_torque);

/* Number of contact begin/end events produced by the last `sim_step`. */
uint32_t sim_events_len(uint32_t h);

/* Event field: 0 = body a, 1 = body b, 2 = begin (1) / end (0). */
uint32_t sim_event_get(uint32_t h, uint32_t i, uint32_t field);

/* Sweep a circle; result fields as `sim_raycast` via `sim_ray_get`. */
uint32_t sim_circlecast(uint32_t h, int64_t x, int64_t y, int64_t dx, int64_t dy, int64_t radius, uint32_t mask);

/* Joint reference scenario: pendulum on a distance joint, a slack rope, a motorised wheel on a static axle and a welded pair falling onto them; 240 ticks. Equal on native and wasm32. */
uint64_t sim_joint_golden_hash(void);

/* Grid navmesh: `cols × rows` cells of size `cell` from (ox, oy); blocked cells are given as (col, row) pairs in the snapshot buffer (u32 LE pairs: fill with `sim_snapshot_set_*`). */
uint32_t sim_nav_grid(uint32_t h, int64_t ox, int64_t oy, int64_t cell, uint32_t cols, uint32_t rows);

/* Load a navmesh from its JSON bytes placed in the snapshot buffer (see `NavMesh::to_bytes`). */
uint32_t sim_nav_load(uint32_t h);

uint64_t sim_nav_hash(uint32_t h);

/* Polygon containing (x, y), or u32::MAX. */
uint32_t sim_nav_locate(uint32_t h, int64_t x, int64_t y);

/* Find a path; returns the number of waypoints stored for `sim_nav_point` (0 = no path). */
uint32_t sim_nav_path(uint32_t h, int64_t x0, int64_t y0, int64_t x1, int64_t y1, uint32_t mask);

/* Coordinate `i` (0 = x0, 1 = y0, 2 = x1, ...) of the last path. */
int64_t sim_nav_point(uint32_t h, uint32_t i);

/* Mesh raycast; returns 1 and stores the hit point in `sim_ray_get(1..=2)` when blocked. */
uint32_t sim_nav_raycast(uint32_t h, int64_t x0, int64_t y0, int64_t x1, int64_t y1);

/* Build a flow field towards (x, y); returns its index (u32::MAX when unreachable). */
uint32_t sim_nav_field(uint32_t h, int64_t x, int64_t y, uint32_t mask);

/* Add a crowd agent; returns its id (0 = no navmesh). */
uint32_t sim_agent_add(uint32_t h, int64_t x, int64_t y, int64_t radius, int64_t max_speed);

void sim_agent_remove(uint32_t h, uint32_t id);

/* Steer towards a point (clears any flow field). */
void sim_agent_target(uint32_t h, uint32_t id, int64_t x, int64_t y);

/* Follow a flow field (clears any point target). */
void sim_agent_follow(uint32_t h, uint32_t id, uint32_t field);

void sim_agent_stop(uint32_t h, uint32_t id);

/* Agent field: 0 = x, 1 = y, 2 = vx, 3 = vy, 4 = arrived (0/1), 5 = polygon. */
int64_t sim_agent_get(uint32_t h, uint32_t id, uint32_t field);

/* Advance the crowd one tick (call alongside `sim_step`). */
void sim_crowd_step(uint32_t h);

uint64_t sim_crowd_hash(uint32_t h);

/* Snapshot the crowd into the snapshot buffer; returns its length. */
uint32_t sim_crowd_snapshot(uint32_t h);

uint32_t sim_crowd_restore(uint32_t h);

/* Navigation reference scenario: 8×6 grid with a wall, 6 agents on a flow field and two head-on agents, 600 ticks; equal on native and wasm32. */
uint64_t sim_nav_golden_hash(void);

/* Create a 3D world: gravity (raw Fx), tick rate; returns a handle. */
uint32_t sim3_world_new(int64_t gx, int64_t gy, int64_t gz, uint32_t tick_hz);

void sim3_world_free(uint32_t h);

void sim3_step(uint32_t h);

uint64_t sim3_tick(uint32_t h);

uint64_t sim3_hash(uint32_t h);

uint32_t sim3_snapshot(uint32_t h);

uint32_t sim3_snapshot_byte(uint32_t h, uint32_t i);

void sim3_snapshot_set_len(uint32_t h, uint32_t len);

void sim3_snapshot_set_byte(uint32_t h, uint32_t i, uint32_t b);

uint32_t sim3_restore(uint32_t h);

uint32_t sim3_add_sphere(uint32_t h, uint32_t k, int64_t x, int64_t y, int64_t z, int64_t radius, int64_t density);

uint32_t sim3_add_box(uint32_t h, uint32_t k, int64_t x, int64_t y, int64_t z, int64_t hx, int64_t hy, int64_t hz, int64_t density);

/* Capsule along the body y axis. */
uint32_t sim3_add_capsule(uint32_t h, uint32_t k, int64_t x, int64_t y, int64_t z, int64_t half_len, int64_t radius, int64_t density);

uint32_t sim3_remove_body(uint32_t h, uint32_t id);

void sim3_set_material(uint32_t h, uint32_t id, int64_t friction, int64_t restitution, uint32_t sensor);

void sim3_set_layers(uint32_t h, uint32_t id, uint32_t layer, uint32_t mask);

void sim3_set_velocity(uint32_t h, uint32_t id, int64_t vx, int64_t vy, int64_t vz, int64_t wx, int64_t wy, int64_t wz);

void sim3_set_position(uint32_t h, uint32_t id, int64_t x, int64_t y, int64_t z);

/* Set orientation as a quaternion (x, y, z, w), normalised on entry. */
void sim3_set_rotation(uint32_t h, uint32_t id, int64_t qx, int64_t qy, int64_t qz, int64_t qw);

/* Impulse at the centre of mass (`at_point` == 0) or at world point (px, py, pz). */
void sim3_apply_impulse(uint32_t h, uint32_t id, int64_t ix, int64_t iy, int64_t iz, uint32_t at_point, int64_t px, int64_t py, int64_t pz);

/* Body state: 0..=2 position, 3..=6 rotation (x, y, z, w), 7..=9 velocity, 10..=12 angular velocity. */
int64_t sim3_body_get(uint32_t h, uint32_t id, uint32_t field);

/* Raycast; returns body id + 1 (0 = miss) and fills `sim3_ray_get`: 0 fraction, 1..=3 point, 4..=6 normal. */
uint32_t sim3_raycast(uint32_t h, int64_t x, int64_t y, int64_t z, int64_t dx, int64_t dy, int64_t dz, uint32_t mask);

int64_t sim3_ray_get(uint32_t field);

uint32_t sim3_events_len(uint32_t h);

/* Event field: 0 = body a, 1 = body b, 2 = begin (1) / end (0). */
uint32_t sim3_event_get(uint32_t h, uint32_t i, uint32_t field);

/* 3D reference scenario: floor, a 3-level box pyramid, a tilted box, a lying capsule and two spheres (one bouncy, one rolling into the pyramid); 240 ticks. Equal on native and wasm32. */
uint64_t sim3_golden_hash(void);

uint32_t sim3_joint_ball(uint32_t h, uint32_t a, uint32_t b, int64_t ax, int64_t ay, int64_t az, int64_t bx, int64_t by, int64_t bz);

/* Hinge around local axes (ua, ub); `has_limits` != 0 enables [lower, upper]; motor when `max_torque` > 0. */
uint32_t sim3_joint_hinge(uint32_t h, uint32_t a, uint32_t b, int64_t ax, int64_t ay, int64_t az, int64_t bx, int64_t by, int64_t bz, int64_t uax, int64_t uay, int64_t uaz, int64_t ubx, int64_t uby, int64_t ubz, uint32_t has_limits, int64_t lower, int64_t upper, int64_t motor_speed, int64_t max_torque);

uint32_t sim3_joint_distance(uint32_t h, uint32_t a, uint32_t b, int64_t ax, int64_t ay, int64_t az, int64_t bx, int64_t by, int64_t bz, int64_t len, int64_t min_len);

uint32_t sim3_joint_remove(uint32_t h, uint32_t id);

void sim3_joint_motor(uint32_t h, uint32_t id, int64_t speed, int64_t max_torque);

/* Hinge angle in radians (raw Fx), 0 for other joints. */
int64_t sim3_hinge_angle(uint32_t h, uint32_t id);

/* Capsule character controller over a kinematic capsule body; returns its index. */
uint32_t sim3_kcc_new(uint32_t h, uint32_t body, int64_t step_height, int64_t min_ground_ny, int64_t snap);

/* Move the controller by (dx, dy, dz) this tick; returns grounded (1/0). Read the body for position. */
uint32_t sim3_kcc_move(uint32_t h, uint32_t k, int64_t dx, int64_t dy, int64_t dz);

/* Controller state: 0..=2 ground normal, 3..=5 velocity, 6 grounded, 7 ground body (+1, 0 = none). */
int64_t sim3_kcc_get(uint32_t h, uint32_t k, uint32_t field);

/* Clear the staged hull. */
void sim3_convex_reset(uint32_t h);

/* Push a body-space vertex (raw Fx); at most 32. */
uint32_t sim3_convex_vert(uint32_t h, int64_t x, int64_t y, int64_t z);

/* Push a vertex index onto the face being built. */
void sim3_convex_index(uint32_t h, uint32_t i);

/* Close the current face (>= 3 indices, any winding); at most 32 faces. */
uint32_t sim3_convex_face_end(uint32_t h);

/* Add a convex body from the staged hull (which stays staged for reuse); returns the body id like `sim3_add_box`, or `u32::MAX` if the hull is invalid (< 4 vertices or faces). */
uint32_t sim3_add_convex(uint32_t h, uint32_t k, int64_t x, int64_t y, int64_t z, int64_t density);

/* Sphere sweep; result fields as `sim3_ray_get`. Returns body id + 1 or 0. */
uint32_t sim3_spherecast(uint32_t h, int64_t x, int64_t y, int64_t z, int64_t dx, int64_t dy, int64_t dz, int64_t radius, uint32_t mask);

/* Convex reference scenario: three pyramids dropped in a column, a box sliding off a static wedge, a sphere rolling onto a pyramid, plus a sphere sweep mixed into the hash; 300 ticks. */
uint64_t sim3_hull_golden_hash(void);

/* 3D joint/character reference scenario: ball pendulum, rope, limited hinge door, motor wheel and a capsule character walking over a step into a wall; 300 ticks. Equal on native and wasm32. */
uint64_t sim3_joint_golden_hash(void);

#ifdef __cplusplus
}
#endif
