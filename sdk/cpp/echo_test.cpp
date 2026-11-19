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
    int hello = 0, ticks = 0, unsequenced = 0;
    b0.onData = [&](Channel channel, uint16_t, const uint8_t* d, size_t n) { EventFrame f; if (EventFrame::decode(d, n, f)) { if (f.code == 1) hello++; else if (f.code == 2) ticks++; else if (f.code == 6 && channel == Channel::UnreliableUnsequenced) unsequenced++; } };
    Client b(std::move(b0)); // a moved client keeps its callbacks
    std::string room = "cpp-echo-" + std::to_string(getpid());
    a.joinOrCreate(room, Mode::Relay, 4); b.joinOrCreate(room, Mode::Relay, 4);
    const uint8_t hi[] = "hello"; a.sendEvent(1, hi, 5, Target::Others, true);
    for (uint8_t i = 0; i < 10; i++) { a.sendEvent(2, &i, 1); a.flush(); std::this_thread::sleep_for(std::chrono::milliseconds(16)); }
    for (int s = 0; s < 300 && !(hello && ticks == 10); s++) { b.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
    std::printf("cpp: hello=%d ticks=%d/10 joined=%s\n", hello, ticks, b.joinedJson().substr(0, 60).c_str());
    // events on the unsequenced channel arrive on it
    for (uint8_t i = 0; i < 10; i++) { a.sendEventOn(Channel::UnreliableUnsequenced, 6, &i, 1); a.flush(); std::this_thread::sleep_for(std::chrono::milliseconds(16)); }
    for (int s = 0; s < 300 && unsequenced < 10; s++) { b.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
    std::printf("cpp unsequenced channel: events=%d/10\n", unsequenced);
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
    // over a slow link (NET_SLOW_HTTP, see README): a client that left a room joins another one
    bool rejoined = true;
    if (const char* slow = std::getenv("NET_SLOW_HTTP")) {
        Client f, g; f.connect(slow, Client::issueGrant(http, key, "cpp-f", "F"), true); g.connect(slow, Client::issueGrant(http, key, "cpp-g", "G"), true);
        int errors = 0, second = 0;
        f.onControl = [&](const std::string& json) { if (Client::jsonStr(json, "t") == "error") errors++; };
        g.onData = [&](Channel, uint16_t, const uint8_t* d, size_t n) { EventFrame frame; if (EventFrame::decode(d, n, frame) && frame.code == 5) second++; };
        f.joinOrCreate(room + "-slow", Mode::Relay, 4); g.joinOrCreate(room + "-slow", Mode::Relay, 4);
        const uint8_t nine = 9;
        // events on their way to `f` when it leaves must not make it answer the room it has left
        auto chatter = [&](int rounds, bool both) { for (int s = 0; s < rounds; s++) { g.sendEvent(4, &nine, 1, Target::Others, true); g.flush(); g.poll(); if (both) { f.flush(); } f.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); } };
        chatter(30, true); f.leave(); chatter(30, false);
        f.joinOrCreate(room + "-slow2", Mode::Relay, 4); g.leave(); g.join(room + "-slow2");
        for (uint8_t i = 0; i < 5; i++) f.sendEvent(5, &i, 1, Target::Others, true);
        for (int s = 0; s < 150 && second < 5; s++) { f.flush(); f.poll(); g.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
        std::printf("cpp slow link: errors after leave=%d, reliable events in the next room=%d/5\n", errors, second);
        rejoined = errors == 0 && second == 5;
    } else {
        std::printf("cpp slow link: skipped (NET_SLOW_HTTP not set)\n");
    }
    a.leave(); return (hello && ticks == 10 && unsequenced == 10 && cached == 1 && history == 1 && early == 0 && refused && rejoined) ? 0 : 1;
}
