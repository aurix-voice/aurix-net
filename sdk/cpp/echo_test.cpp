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
    if (a.playerId() != "cpp-a" || a.playerName() != "A" || b0.playerId() != "cpp-b") { std::printf("cpp player: id=%s name=%s -> FAIL\n", a.playerId().c_str(), a.playerName().c_str()); return 1; }
    int hello = 0, ticks = 0, unsequenced = 0;
    b0.onData = [&](Channel channel, uint16_t, const uint8_t* d, size_t n) { EventFrame f; if (EventFrame::decode(d, n, f)) { if (f.code == 1) hello++; else if (f.code == 2) ticks++; else if (f.code == 6 && channel == Channel::UnreliableUnsequenced) unsequenced++; } };
    Client b(std::move(b0)); // a moved client keeps its callbacks
    std::string room = "cpp-echo-" + std::to_string(getpid());
    a.joinOrCreate(room, Mode::Relay, 4); b.joinOrCreate(room, Mode::Relay, 4);
    const uint8_t hi[] = "hello"; a.sendEvent(1, hi, 5, Target::Others, true);
    for (uint8_t i = 0; i < 10; i++) { a.sendEvent(2, &i, 1); a.flush(); std::this_thread::sleep_for(std::chrono::milliseconds(16)); }
    for (int s = 0; s < 300 && !(hello && ticks == 10); s++) { b.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
    std::printf("cpp: hello=%d ticks=%d/10 joined=%s\n", hello, ticks, b.joinedJson().substr(0, 60).c_str());
    // the room as the node described it at the join: its size and who was in it, the joiner too
    std::string facts = b.joinedJson();
    bool described = facts.find("\"max_players\":4") != std::string::npos && facts.find("\"player_id\":\"cpp-a\"") != std::string::npos && facts.find("\"player_id\":\"cpp-b\"") != std::string::npos;
    std::printf("cpp joined: the room's size and both players=%d\n", described);
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
    // a message with a field the node does not know is refused before it is sent, and the field is named
    int unknown = 0; std::string named;
    try { e.sendControl("{\"t\":\"set_groups\",\"join\":[1],\"colour\":\"red\"}"); } catch (const Error& error) { unknown = error.code; named = error.what(); }
    std::printf("cpp refused joins: missing room code=%d, full room code=%d; unknown field code=%d (%s)\n", missing, full, unknown, named.c_str());
    bool refused = missing == AURIX_NET_ERR_NOT_FOUND && full == AURIX_NET_ERR_ROOM_FULL
        && unknown == AURIX_NET_ERR_INVALID_REQUEST && named.find("unknown field `colour`") != std::string::npos;
    // a random join finds a room by the props it publishes, and a seat held for a player is kept for it
    Client h; h.connect(http, Client::issueGrant(http, key, "cpp-h", "H"));
    h.joinOrCreate(room + "-open", Mode::Relay, 4, "{\"map\":" + Client::quote(room) + "}", {"map"});
    int random = e.joinRandom("", "map = '" + room + "'"), taken = 0;
    e.leave(); h.leave(); d.leave();
    d.joinOrCreate(room + "-party", Mode::Relay, 2, "", {}, {"cpp-e"});
    try { h.join(room + "-party"); } catch (const Error& error) { taken = error.code; }
    int awaited = e.join(room + "-party");
    std::printf("cpp random join: actor=%d; held seat: another player code=%d, the awaited one actor=%d\n", random, taken, awaited);
    bool matched = random == 2 && taken == AURIX_NET_ERR_ROOM_FULL && awaited == 2;
    // a hidden room is not offered to a random join and is entered by its name; a closed room lets its creator in and nobody else
    Client hider, closer, guest;
    hider.connect(http, Client::issueGrant(http, key, "cpp-hider", "Hider")); closer.connect(http, Client::issueGrant(http, key, "cpp-closer", "Closer")); guest.connect(http, Client::issueGrant(http, key, "cpp-guest", "Guest"));
    int offered = 0, closedJoin = 0, closedCreate = 0;
    hider.joinOrCreate(room + "-hidden", Mode::Relay, 4, "{\"map\":" + Client::quote(room + "-hidden") + "}", {"map"}, {}, false);
    closer.joinOrCreate(room + "-closed", Mode::Relay, 4, "", {}, {}, true, false);
    try { guest.joinRandom("", "map = '" + room + "-hidden'"); } catch (const Error& error) { offered = error.code; }
    try { guest.join(room + "-closed"); } catch (const Error& error) { closedJoin = error.code; }
    try { guest.joinOrCreate(room + "-closed", Mode::Relay, 4); } catch (const Error& error) { closedCreate = error.code; }
    int byName = guest.join(room + "-hidden");
    std::printf("cpp hidden room: random join code=%d, by name actor=%d; closed room: join code=%d, join_or_create code=%d\n", offered, byName, closedJoin, closedCreate);
    bool shaped = offered == AURIX_NET_ERR_NOT_FOUND && byName == 2 && closedJoin == AURIX_NET_ERR_ROOM_CLOSED && closedCreate == AURIX_NET_ERR_ROOM_CLOSED;
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
        rejoined = !std::getenv("NET_LIVE_REQUIRED");
    }
    // over a link that takes 100 ms each way and closes on `stats` (NET_BREAK_HTTP, see README): the
    // reliable events the node did not acknowledge come before the room is lost, and not to a client that leaves
    bool handedBack = true;
    if (const char* broken = std::getenv("NET_BREAK_HTTP")) {
        for (std::string how : {"kick", "cut", "leave"}) {
            Client master, c; master.connect(http, Client::issueGrant(http, key, "cpp-m-" + how, "M")); c.connect(broken, Client::issueGrant(http, key, "cpp-u-" + how, "U"), true);
            int first = 0; std::string told;
            master.onData = [&](Channel, uint16_t, const uint8_t* d, size_t n) { EventFrame f; if (EventFrame::decode(d, n, f) && f.code == 1) first++; };
            c.onUndelivered = [&](const std::vector<UndeliveredEvent>& events) { for (const auto& e : events) told += std::to_string(e.code) + ":" + std::to_string(e.payload.size()) + ":" + std::to_string((int)e.target) + ":" + std::to_string(e.actors.size()) + ":" + std::to_string(e.group) + ":" + std::to_string(e.cache) + " "; told += "| "; };
            c.onControl = [&](const std::string& json) { if (Client::jsonStr(json, "t") == "left") told += "left " + Client::jsonStr(json, "reason") + " | "; };
            c.onClosed = [&](const std::string&) { told += "closed | "; };
            master.joinOrCreate(room + "-" + how, Mode::Relay, 4); int actor = c.join(room + "-" + how);
            const uint8_t one = 1; c.sendEvent(1, &one, 1, Target::Others, true);
            for (int s = 0; s < 300 && !first; s++) { c.flush(); c.poll(); master.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
            for (int s = 0; s < 40; s++) { c.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
            std::vector<uint8_t> big(3000, 7);
            c.sendEventEx(2, &one, 1, Target::Actors, true, {7, 9}, -1, 0); c.sendEvent(3, &one, 1);
            c.sendEventEx(5, big.data(), big.size(), Target::Master, true, {}, -1, 1); c.sendEventEx(6, &one, 1, Target::All, true, {}, 9, 0); c.flush();
            if (how == "kick") master.sendControl("{\"t\":\"kick\",\"actor\":" + std::to_string(actor) + "}");
            else if (how == "cut") c.sendControl("{\"t\":\"stats\"}");
            else c.leave();
            for (int s = 0; s < 100; s++) { try { c.poll(); } catch (const Error&) {} master.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
            const std::string events = "2:1:3:2:-1:0 5:3000:2:0:-1:1 6:1:0:0:9:0 | ";
            std::string want = how == "kick" ? events + "left kicked | " : how == "cut" ? events + "closed | " : "left left | ";
            std::printf("cpp undelivered events, %s: [%s] -> %s\n", how.c_str(), told.c_str(), told == want ? "ok" : "FAIL");
            handedBack = handedBack && told == want;
        }
    } else {
        std::printf("cpp broken link: skipped (NET_BREAK_HTTP not set)\n");
        handedBack = !std::getenv("NET_LIVE_REQUIRED");
    }
    a.leave(); return (hello && ticks == 10 && unsequenced == 10 && cached == 1 && history == 1 && early == 0 && described && refused && matched && shaped && rejoined && handedBack) ? 0 : 1;
}
