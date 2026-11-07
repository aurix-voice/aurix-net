// Offline check of the aurix-sim C ABI from libaurix_net: golden hashes must equal the Rust /
// wasm32 values, and a tiny 2D and 3D scene must behave. Build from the repo root:
//   cargo build --release -p net-ffi
//   gcc -std=c11 -Wall -Wextra crates/net-ffi/ctest/sim.c -Ltarget/release -laurix_net -lm
//       -Wl,-rpath,$PWD/target/release -o /tmp/sim_ctest && /tmp/sim_ctest
#include "../include/aurix_sim.h"
#include <inttypes.h>
#include <math.h>
#include <stdio.h>

static int fails;
#define CHECK(c, ...) do { if (c) printf("ok   " __VA_ARGS__); else { fails++; printf("FAIL " __VA_ARGS__); } printf("\n"); } while (0)
#define FX(v) aurix_fx_from_double(v)

int main(void) {
    CHECK(sim_golden_hash() == 0x58d9870ba45952b5ULL, "2D physics golden %#" PRIx64, sim_golden_hash());
    CHECK(sim_joint_golden_hash() == 0x4bcc781a484ceeedULL, "2D joints golden %#" PRIx64, sim_joint_golden_hash());
    CHECK(sim_nav_golden_hash() == 0x746c5a608abe2f26ULL, "nav golden %#" PRIx64, sim_nav_golden_hash());
    CHECK(sim3_golden_hash() == 0x0a9c7e6fc4fa43bfULL, "3D physics golden %#" PRIx64, sim3_golden_hash());
    CHECK(sim3_joint_golden_hash() == 0x5db56af63cceee85ULL, "3D joints golden %#" PRIx64, sim3_joint_golden_hash());
    CHECK(sim3_hull_golden_hash() == 0x29ab0385d38dc53fULL, "3D hulls golden %#" PRIx64, sim3_hull_golden_hash());

    uint32_t w = sim_world_new(0, FX(-10), 60);
    sim_add_box(w, 0, 0, FX(-0.5), FX(20), FX(0.5), AURIX_FX_ONE);
    uint32_t ball = sim_add_circle(w, 2, 0, FX(3), FX(0.5), AURIX_FX_ONE);
    for (int i = 0; i < 240; i++) sim_step(w);
    double y = aurix_fx_to_double(sim_body_get(w, ball, 1));
    CHECK(fabs(y - 0.5) < 0.05, "2D ball rests on the floor (y=%.3f)", y);
    uint64_t h1 = sim_hash(w);
    sim_world_free(w);

    uint32_t w3 = sim3_world_new(0, FX(-10), 0, 60);
    sim3_add_box(w3, 0, 0, FX(-0.5), 0, FX(20), FX(0.5), FX(20), AURIX_FX_ONE);
    uint32_t s = sim3_add_sphere(w3, 2, 0, FX(3), 0, FX(0.5), AURIX_FX_ONE);
    for (int i = 0; i < 240; i++) sim3_step(w3);
    double y3 = aurix_fx_to_double(sim3_body_get(w3, s, 1));
    CHECK(fabs(y3 - 0.5) < 0.05, "3D sphere rests on the floor (y=%.3f)", y3);
    uint32_t hit = sim3_spherecast(w3, FX(0), FX(5), FX(0), FX(0), FX(-10), FX(0), FX(0.25), 0xffffffffu);
    double py = aurix_fx_to_double(sim3_ray_get(2));
    CHECK(hit == s + 1 && fabs(py - 1.0) < 0.05, "3D sphere cast hits the sphere top (y=%.3f)", py);
    sim3_world_free(w3);
    CHECK(h1 != 0, "hash is non-zero");
    printf(fails ? "%d FAILED\n" : "all sim C ABI checks passed\n", fails);
    return fails ? 1 : 0;
}
