#include "aurix_net.hpp"
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <map>
#include <thread>
#include <unistd.h>
using namespace aurix::net;
// Two clients of one lockstep room get the same inputs for every tick, in order, and the same seed.
struct Peer {
    Client client; Lockstep lockstep; std::map<uint32_t, std::string> ticks; int desyncs = 0; bool gap = false; size_t commands = 0; std::vector<std::string> errors; uint64_t hash = 14695981039346656037ull;
    void start(const std::string& http, const char* key, const char* player, const std::string& room) {
        client.connect(http, Client::issueGrant(http, key, player, player));
        client.onData = [this](Channel, uint16_t, const uint8_t* d, size_t n) { lockstep.receive(client, d, n); };
        client.onControl = [this](const std::string& json) {
            std::string type = Client::jsonStr(json, "t");
            if (type == "desync") desyncs++;
            if (type == "error") errors.push_back(Client::jsonStr(json, "code") + ": " + Client::jsonStr(json, "message"));
        };
        client.joinOrCreate(room, Mode::Lockstep, 4, "{\"tick_hz\":30}");
    }
    void step(uint8_t input) {
        lockstep.sendInput(client, &input, 1); client.flush(); client.poll();
        for (auto e = lockstep.poll(); e.kind != Lockstep::Kind::None; e = lockstep.poll()) {
            if (e.kind != Lockstep::Kind::Tick) continue;
            if (!ticks.empty() && e.tick != ticks.rbegin()->first + 1) gap = true;
            std::string& line = ticks[e.tick];
            for (const auto& in : e.inputs) {
                line += std::to_string(in.actor) + "=" + std::to_string(in.len ? in.data[0] : -1) + ",";
                if (in.actor == AURIX_NET_COMMAND_ACTOR) commands += in.len;
            }
            for (char c : line) hash = (hash ^ uint8_t(c)) * 1099511628211ull;
            if (e.tick % 10 == 0) Lockstep::reportHash(client, e.tick, hash);
        }
    }
};
int main() {
    const char* key = std::getenv("NET_API_KEY"); if (!key) { std::fprintf(stderr, "NET_API_KEY missing\n"); return 2; }
    const char* env = std::getenv("NET_HTTP"); std::string http = env ? env : "http://127.0.0.1:8090";
    std::string room = "cpp-ls-" + std::to_string(getpid());
    Peer a, b; a.start(http, key, "cpp-la", room); b.start(http, key, "cpp-lb", room);
    // the longest command reaches both whole, under its 4-byte header; a longer one is refused
    a.lockstep.sendCommand(a.client, std::vector<uint8_t>(AURIX_NET_MAX_COMMAND, 7));
    bool refused = false;
    try { a.lockstep.sendCommand(a.client, std::vector<uint8_t>(AURIX_NET_MAX_COMMAND + 1, 7)); } catch (const Error&) { refused = true; }
    for (int frame = 0; frame < 180; frame++) { a.step(uint8_t(frame)); b.step(uint8_t(255 - frame % 200)); std::this_thread::sleep_for(std::chrono::milliseconds(16)); }
    bool commands = refused && a.commands == 4 + AURIX_NET_MAX_COMMAND && b.commands == a.commands;
    int common = 0, same = 0, both = 0;
    for (const auto& [tick, line] : a.ticks) {
        auto other = b.ticks.find(tick); if (other == b.ticks.end()) continue;
        common++; same += other->second == line; both += line.find(',') != line.rfind(',');
    }
    // a command the node refuses comes back as a control error: one sent past the sender's check,
    // then 40 of 200 bytes in one go, of which a tick carries one and 4096 bytes may wait
    bool empty = false;
    try { a.lockstep.sendCommand(a.client, {}); } catch (const Error&) { empty = true; }
    std::vector<uint8_t> tooLong(2 + AURIX_NET_MAX_COMMAND, 0); tooLong[0] = 0x28;
    a.client.sendData(Channel::Reliable, tooLong.data(), tooLong.size());
    size_t before = b.commands;
    for (int i = 0; i < 40; i++) a.lockstep.sendCommand(a.client, std::vector<uint8_t>(200, uint8_t(i)));
    for (int frame = 0; frame < 120; frame++) { a.step(1); b.step(2); std::this_thread::sleep_for(std::chrono::milliseconds(16)); }
    size_t carried = (b.commands - before) / 204, limited = 0;
    for (size_t i = 1; i < a.errors.size(); i++) limited += a.errors[i] == "rate_limited: rate limited";
    bool refusals = empty && !a.errors.empty() && a.errors[0] == "invalid_request: invalid request: a lockstep command takes 1 to 251 bytes, got 252"
        && limited > 0 && limited + 1 == a.errors.size() && carried + limited == 40;
    auto seedA = a.client.seed(), seedB = b.client.seed();
    a.client.leave(); bool left = !a.client.seed() && a.client.joinedJson().empty() && a.client.actor() == -1;
    // the same player joins from a second connection: the node takes the first one out of the room
    Client second; second.connect(http, Client::issueGrant(http, key, "cpp-lb", "cpp-lb")); second.join(room);
    for (int i = 0; i < 100 && b.client.actor() != -1; i++) { b.client.poll(); std::this_thread::sleep_for(std::chrono::milliseconds(10)); }
    bool replaced = !b.client.seed() && b.client.joinedJson().empty() && b.client.actor() == -1;
    bool ok = left && replaced && commands && refusals && common >= 60 && same == common && both >= 30 && !a.gap && !b.gap && a.desyncs + b.desyncs == 0 && seedA && seedA == seedB;
    std::printf("cpp lockstep: ticks=%zu/%zu common=%d same=%d with both inputs=%d gaps=%d desyncs=%d seed=%s left=%d replaced=%d commands=%d refusals=%d (carried %zu, rate_limited %zu) -> %s\n", a.ticks.size(), b.ticks.size(), common, same, both, a.gap + b.gap, a.desyncs + b.desyncs, seedA && seedA == seedB ? "same" : "differs", left, replaced, int(commands), int(refusals), carried, limited, ok ? "ok" : "FAILED");
    return ok ? 0 : 1;
}
