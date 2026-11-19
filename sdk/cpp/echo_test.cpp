#include "aurix_net.hpp"
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <thread>
#include <unistd.h>
using namespace aurix::net;
int main() {
    const char* key = std::getenv("NET_API_KEY"); if (!key) { std::fprintf(stderr, "NET_API_KEY missing\n"); return 2; }
    const char* env = std::getenv("NET_HTTP"); std::string http = env ? env : "http://127.0.0.1:8090";
    Client a, b0; a.connect(http, Client::issueGrant(http, key, "cpp-a", "A")); b0.connect(http, Client::issueGrant(http, key, "cpp-b", "B"), true);
    int hello = 0, ticks = 0;
    b0.onData = [&](Channel, uint16_t, const uint8_t* d, size_t n) { EventFrame f; if (EventFrame::decode(d, n, f)) { if (f.code == 1) hello++; else if (f.code == 2) ticks++; } };
    Client b(std::move(b0)); // a moved client keeps its callbacks
    std::string room = "cpp-echo-" + std::to_string(getpid());
    a.joinOrCreate(room, Mode::Relay, 4); b.joinOrCreate(room, Mode::Relay, 4);
    const uint8_t hi[] = "hello"; a.sendEvent(1, hi, 5, Target::Others, true);
    for (uint8_t i = 0; i < 10; i++) { a.sendEvent(2, &i, 1); a.flush(); std::this_thread::sleep_for(std::chrono::milliseconds(16)); }
    for (int s = 0; s < 300 && !(hello && ticks == 10); s++) { b.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
    std::printf("cpp: hello=%d ticks=%d/10 joined=%s\n", hello, ticks, b.joinedJson().substr(0, 60).c_str());
    // a late joiner gets the cached events, and the chat history once it joins the channel
    Chat chatA(a); chatA.join(1); const uint8_t seven = 7; a.sendEventEx(3, &seven, 1, Target::Others, true, {}, -1, 1); chatA.send(1, "hello");
    for (int s = 0; s < 30; s++) { a.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
    Client c; c.connect(http, Client::issueGrant(http, key, "cpp-c", "C")); Chat chatC(c); int cached = 0, history = 0;
    chatC.onMessage = [&](const Chat::Message& m) { if (m.text == "hello") history++; };
    c.onData = [&](Channel, uint16_t, const uint8_t* d, size_t n) { EventFrame f; if (EventFrame::decode(d, n, f)) { if (f.code == 3) cached++; chatC.onEventFrame(f); } };
    c.join(room);
    for (int s = 0; s < 30; s++) { c.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
    int early = history; chatC.join(1);
    std::printf("cpp late joiner: cached=%d/1 chat history=%d/1 (%d before the channel join)\n", cached, history, early);
    // a refused join says why in Error::code
    Client d, e; d.connect(http, Client::issueGrant(http, key, "cpp-d", "D")); e.connect(http, Client::issueGrant(http, key, "cpp-e", "E"));
    int missing = 0, full = 0;
    try { d.join(room + "-missing"); } catch (const Error& error) { missing = error.code; }
    d.joinOrCreate(room + "-one", Mode::Relay, 1);
    try { e.join(room + "-one"); } catch (const Error& error) { full = error.code; }
    std::printf("cpp refused joins: missing room code=%d, full room code=%d\n", missing, full);
    bool refused = missing == AURIX_NET_ERR_NOT_FOUND && full == AURIX_NET_ERR_ROOM_FULL;
    a.leave(); return (hello && ticks == 10 && cached == 1 && history == 1 && early == 0 && refused) ? 0 : 1;
}
