#include "aurix_net.hpp"
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <thread>
#include <unistd.h>
using namespace aurix::net;
int main() {
    const char* key = std::getenv("NET_API_KEY"); if (!key) { std::fprintf(stderr, "NET_API_KEY missing\n"); return 2; }
    std::string http = "http://127.0.0.1:8090";
    Client a, b; a.connect(http, Client::issueGrant(http, key, "cpp-a", "A")); b.connect(http, Client::issueGrant(http, key, "cpp-b", "B"), true);
    std::string room = "cpp-echo-" + std::to_string(getpid());
    a.joinOrCreate(room, Mode::Relay, 4); b.joinOrCreate(room, Mode::Relay, 4);
    int hello = 0, ticks = 0;
    b.onData = [&](Channel, uint16_t, const uint8_t* d, size_t n) { EventFrame f; if (EventFrame::decode(d, n, f)) { if (f.code == 1) hello++; else if (f.code == 2) ticks++; } };
    const uint8_t hi[] = "hello"; a.sendEvent(1, hi, 5, Target::Others, true);
    for (uint8_t i = 0; i < 10; i++) { a.sendEvent(2, &i, 1); a.flush(); std::this_thread::sleep_for(std::chrono::milliseconds(16)); }
    for (int s = 0; s < 300 && !(hello && ticks == 10); s++) { b.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
    std::printf("cpp: hello=%d ticks=%d/10 joined=%s\n", hello, ticks, b.joinedJson().substr(0, 60).c_str());
    a.leave(); return (hello && ticks == 10) ? 0 : 1;
}
