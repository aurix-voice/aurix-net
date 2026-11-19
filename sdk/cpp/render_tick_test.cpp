// Offline check of the render tick of aurix::net::Sync: deltas with no entities go to a world with
// no client behind it, late in one run and from a room that stood still in the other.
#include "aurix_net.hpp"
#include <cmath>
#include <cstdio>
using namespace aurix::net;
static const double FRAME = 1.0 / 60;
static void delta(Sync& world, uint32_t tick) {
    const uint8_t frame[13] = {0x11, uint8_t(tick >> 24), uint8_t(tick >> 16), uint8_t(tick >> 8), uint8_t(tick)};
    world.receive(frame, sizeof frame);
}
static bool near(double a, double b) { return std::fabs(a - b) < 1e-6; }
int main() {
    const std::string SCHEMA = "component T {\n  x: f32\n}\n";
    Sync late(SCHEMA, 60, 2);
    delta(late, 10);
    late.advance(FRAME);
    bool waited = true;
    for (int frame = 0; frame < 3; frame++) waited = late.advance(FRAME) <= late.serverTick() && waited;
    double ahead = late.renderTick();
    for (uint32_t tick = 11; tick <= 14; tick++) delta(late, tick);
    bool ok = waited && near(late.renderTick(), 12);
    std::printf("cpp render tick, late deltas: %.1f at serverTick=10, then %.1f at serverTick=%u -> %s\n", ahead, late.renderTick(), late.serverTick(), ok ? "ok" : "FAIL");

    Sync still(SCHEMA, 60, 2);
    delta(still, 10);
    for (int frame = 0; frame < 7; frame++) still.advance(FRAME);
    double held = still.renderTick();
    bool back = held == 10;
    for (uint32_t tick = 11; tick <= 20; tick++) {
        delta(still, tick);
        bool behind = near(still.renderTick(), tick - 2);
        back = still.advance(FRAME) <= tick && behind && back;
    }
    std::printf("cpp render tick, room stood still: %.1f at serverTick=10, then %.1f at serverTick=%u -> %s\n", held, still.renderTick(), still.serverTick(), back ? "ok" : "FAIL");
    return ok && back ? 0 : 1;
}
