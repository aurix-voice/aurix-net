// Live check of aurix::net::Sync: two clients in an owner-authoritative sync room, A writes its
// Transform every tick, B receives the replicated entity. NET_API_KEY (+ NET_HTTP) required.
#include "aurix_net.hpp"
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <thread>
#include <unistd.h>
using namespace aurix::net;
int main() {
    const char* key = std::getenv("NET_API_KEY"); if (!key) { std::fprintf(stderr, "NET_API_KEY missing\n"); return 2; }
    const char* env = std::getenv("NET_HTTP"); std::string http = env ? env : "http://127.0.0.1:8090";
    const std::string SCHEMA = "component Transform {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n}\n";
    Client a, b;
    a.connect(http, Client::issueGrant(http, key, "cpp-sync-a", "A"));
    b.connect(http, Client::issueGrant(http, key, "cpp-sync-b", "B"), true);
    std::string room = "cpp-sync-" + std::to_string(getpid());
    std::string props = "{\"schema\":" + Client::quote(SCHEMA) + "}";
    int actorA = a.joinOrCreate(room, Mode::Sync, 4, props); b.joinOrCreate(room, Mode::Sync, 4, props);
    Sync sa(SCHEMA), sb(SCHEMA);
    int tr = sa.component("Transform"), pos = sa.field(tr, "pos"), rot = sa.field(tr, "rot");
    a.onData = [&](Channel, uint16_t, const uint8_t* d, size_t n) { sa.receive(a, d, n); };
    b.onData = [&](Channel, uint16_t, const uint8_t* d, size_t n) { sb.receive(b, d, n); };
    float x = 0; int updates = 0; float last[4] = {0, 0, 0, 0};
    for (int i = 0; i < 90; i++) {
        x += 0.5f; float p[3] = {x, 1, -2}, q[4] = {0, 0, 0, 1};
        sa.begin(tr).write(pos, p, 3).write(rot, q, 4).end().sendWrites(a); a.flush();
        std::this_thread::sleep_for(std::chrono::milliseconds(16));
        a.poll(); b.poll(); sb.advance(0.016);
        for (auto e = sb.poll(); e.kind != Sync::Kind::None; e = sb.poll())
            if (e.owner == actorA && e.kind != Sync::Kind::Despawn && sb.vector(e.entity, tr, pos, last) == 3) updates++;
    }
    int own = 0; uint64_t id; int owner;
    for (int i = 0; sa.entityAt(i, id, owner); i++) if (owner == actorA) own++;
    bool ok = sb.serverTick() > 30 && sb.gaps() == 0 && updates >= 20 && std::fabs(last[0] - x) < 3 && std::fabs(last[1] - 1) < 0.02 && std::fabs(last[2] + 2) < 0.02 && own == 1
              && sb.renderTick() > 0 && sb.renderTick() <= sb.serverTick();
    std::printf("cpp sync: serverTick=%u gaps=%llu updates=%d last=(%.2f %.2f %.2f) x=%.1f own=%d renderTick=%.1f -> %s\n", sb.serverTick(), (unsigned long long)sb.gaps(), updates, last[0], last[1], last[2], x, own, sb.renderTick(), ok ? "ok" : "FAIL");
    a.leave(); b.leave(); return ok ? 0 : 1;
}
