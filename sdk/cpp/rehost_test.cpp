// Live rehost check (run by scripts/sdk-rehost-test.sh): needs two nodes (scripts/two-nodes.sh) and
// NET_API_KEY. Joins a relay room on node A; a client that asks node B for that room is sent to A.
// Then waits for `rehost` (A got SIGTERM), comes back through node B with the same grant, and a
// second client on B must still reach it.
#include "aurix_net.hpp"
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <set>
#include <thread>
#include <unistd.h>
using namespace aurix::net;
int main(int argc, char** argv) {
    const char* key = std::getenv("NET_API_KEY"); if (!key) { std::fprintf(stderr, "NET_API_KEY missing\n"); return 2; }
    const std::string A = "http://127.0.0.1:8090", B = "http://127.0.0.1:8091";
    std::string room = argc > 1 ? argv[1] : "cpp-rehost-" + std::to_string(getpid());
    std::string grant = Client::issueGrant(A, key, "cpp-rehost-a", "A");
    Client a; a.connect(A, grant);
    int actor = a.joinOrCreate(room, Mode::Relay, 8);
    std::printf("cpp: joined %s actor=%d on A\n", room.c_str(), actor);
    {
        Client other; int there = other.connectAndJoin(B, Client::issueGrant(B, key, "cpp-rehost-c", "C"), "{\"t\":\"join\",\"name\":" + Client::quote(room) + "}");
        int hello = 0;
        a.onData = [&](Channel, uint16_t, const uint8_t* d, size_t n) { EventFrame f; if (EventFrame::decode(d, n, f) && f.code == 6) hello++; };
        const uint8_t hi[] = "hi"; other.sendEvent(6, hi, 2, Target::Others, true); other.flush();
        for (int i = 0; i < 300 && !hello; i++) { a.poll(); other.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
        std::printf("cpp: asked B for the room on A: actor=%d reached=%d\n", there, hello);
        if (!hello) { std::printf("CPP_REHOST_FAIL the client sent to the room's node did not reach it\n"); return 1; }
        other.leave(); a.onData = nullptr;
    }
    bool rehost = false; a.onRehost = [&] { rehost = true; };
    // a reliable event the room did not get is handed back: none is lost without a word
    Client listener; listener.connectAndJoin(A, Client::issueGrant(A, key, "cpp-rehost-l", "L"), "{\"t\":\"join\",\"name\":" + Client::quote(room) + "}");
    std::set<uint32_t> got, handedBack; uint32_t sent = 0;
    auto number = [](const uint8_t* bytes) { uint32_t n; std::memcpy(&n, bytes, 4); return n; };
    listener.onData = [&](Channel, uint16_t, const uint8_t* d, size_t n) { EventFrame f; if (EventFrame::decode(d, n, f) && f.code == 3 && f.len == 4) got.insert(number(f.payload)); };
    a.onUndelivered = [&](const std::vector<UndeliveredEvent>& events) { for (const auto& event : events) handedBack.insert(number(event.payload.data())); };
    for (int i = 0; i < 6000 && !rehost; i++) {
        if (i % 2 == 0) { a.sendEvent(3, (const uint8_t*)&sent, 4, Target::Others, true); a.flush(); sent++; }
        a.poll(); listener.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    if (!rehost) { std::printf("CPP_REHOST_FAIL no rehost within 60 s\n"); return 1; }
    for (int i = 0; i < 50; i++) { listener.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
    int neither = 0, both = 0;
    for (uint32_t n = 0; n < sent; n++) { neither += !got.count(n) && !handedBack.count(n); both += got.count(n) && handedBack.count(n); }
    std::printf("cpp: reliable events at rehost: sent %u, the room got %zu, handed back %zu, neither %d, both %d\n", sent, got.size(), handedBack.size(), neither, both);
    if (neither) { std::printf("CPP_REHOST_FAIL a reliable event was lost without a word\n"); return 1; }
    if (a.actor() != -1 || !a.joinedJson().empty()) { std::printf("CPP_REHOST_FAIL still describes the room the node handed off\n"); return 1; }
    Client a2; a2.connect(B, grant);
    int again = a2.join(room);
    std::printf("cpp: rejoined actor=%d (same actor: %s)\n", again, again == actor ? "true" : "false");
    bool reached = false;
    a2.onData = [&](Channel, uint16_t, const uint8_t* d, size_t n) { EventFrame f; if (EventFrame::decode(d, n, f) && f.code == 7) reached = true; };
    Client b; b.connect(B, Client::issueGrant(B, key, "cpp-rehost-b", "B"));
    b.join(room);
    const uint8_t after[] = "after-rehost"; b.sendEvent(7, after, 12, Target::Others, true); b.flush();
    for (int i = 0; i < 300 && !reached; i++) { a2.poll(); b.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
    bool ok = reached && again == actor;
    std::printf(ok ? "CPP_REHOST_OK\n" : "CPP_REHOST_FAIL reached=%d\n", reached);
    return ok ? 0 : 1;
}
