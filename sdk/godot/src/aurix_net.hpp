// Header-only C++17 wrapper over libaurix_net (aurix_net.h). Used by the Unreal and Godot SDKs.
#pragma once
#include "aurix_net.h"
#include <cstdint>
#include <functional>
#include <stdexcept>
#include <string>
#include <vector>

namespace aurix::net {

enum class Channel : uint8_t { Unreliable = 0, Reliable = 1, ReliableUnordered = 2 };
enum class Target : int32_t { All = 0, Others = 1, Master = 2, Actors = 3 };
enum class Mode { Relay, Sync, Lockstep };

struct Error : std::runtime_error { using std::runtime_error::runtime_error; };

inline std::string lastError() { const char* e = aurix_net_last_error(); return e ? e : "unknown error"; }

// Relay event frame: u8 code, u8 flags, u16 from (BE), [u8 group], [u16 n, n*u16], payload.
struct EventFrame {
    uint8_t code = 0; uint16_t from = 0; bool hasGroup = false; uint8_t group = 0;
    const uint8_t* payload = nullptr; size_t len = 0;
    static bool decode(const uint8_t* b, size_t n, EventFrame& f) {
        if (!b || n < 4) return false;
        f.code = b[0]; uint8_t flags = b[1]; f.from = uint16_t((b[2] << 8) | b[3]); size_t o = 4;
        if (flags & 0x10) { if (n < o + 1) return false; f.hasGroup = true; f.group = b[o++]; }
        if ((flags & 3) == 3) { if (n < o + 2) return false; size_t c = (b[o] << 8) | b[o + 1]; o += 2 + c * 2; if (n < o) return false; }
        f.payload = b + o; f.len = n - o; return true;
    }
};

class Client {
public:
    std::function<void(const std::string& json)> onControl;
    std::function<void(Channel, uint16_t tick, const uint8_t*, size_t)> onData;
    std::function<void()> onDataReady;
    std::function<void(const std::string& reason)> onClosed;

    Client() = default;
    Client(const Client&) = delete; Client& operator=(const Client&) = delete;
    Client(Client&& o) noexcept : h_(o.h_), actor_(o.actor_) { o.h_ = nullptr; }
    ~Client() { destroy(); }

    static std::string version() { return aurix_net_version(); }
    /// Backend/tooling only.
    static std::string issueGrant(const std::string& http, const std::string& apiKey, const std::string& player, const std::string& name) {
        char* g = aurix_net_issue_grant(http.c_str(), apiKey.c_str(), player.c_str(), name.c_str());
        if (!g) throw Error(lastError());
        std::string s(g); aurix_net_string_free(g); return s;
    }

    void connect(const std::string& http, const std::string& grant, bool tunnel = false) {
        destroy();
        h_ = aurix_net_connect(http.c_str(), grant.c_str(), tunnel ? 1 : 0);
        if (!h_) throw Error(lastError());
    }
    bool connected() const { return h_ != nullptr; }
    int actor() const { return actor_; }

    /// Protocol JSON, e.g. {"t":"set_room_props","props":{...}}.
    void sendControl(const std::string& json) { if (aurix_net_send_control(h_, json.c_str()) != 0) throw Error(lastError()); }

    int joinOrCreate(const std::string& name, Mode mode = Mode::Relay, int maxPlayers = 8, const std::string& propsJson = "") {
        std::string j = "{\"t\":\"join_or_create\",\"name\":" + quote(name) + ",\"mode\":\"" + modeName(mode) + "\",\"max_players\":" + std::to_string(maxPlayers);
        if (!propsJson.empty()) j += ",\"props\":" + propsJson;
        return joinJson(j + "}");
    }
    int join(const std::string& name, const std::string& schemaHash = "") {
        std::string j = "{\"t\":\"join\",\"name\":" + quote(name);
        if (!schemaHash.empty()) j += ",\"schema_hash\":" + quote(schemaHash);
        return joinJson(j + "}");
    }
    int joinJson(const std::string& json) {
        int a = aurix_net_join(h_, json.c_str());
        if (a < 0) throw Error(lastError());
        actor_ = a; return a;
    }
    std::string joinedJson() const { char* s = aurix_net_joined_json(h_); if (!s) return ""; std::string r(s); aurix_net_string_free(s); return r; }

    void sendEvent(uint8_t code, const uint8_t* data, size_t len, Target target = Target::Others, bool reliable = false, const std::vector<uint16_t>& actors = {}) {
        if (aurix_net_send_event(h_, code, int32_t(target), actors.empty() ? nullptr : actors.data(), actors.size(), data, len, reliable ? 1 : 0) != 0) throw Error(lastError());
    }
    void sendData(Channel ch, const uint8_t* data, size_t len) { if (aurix_net_send_data(h_, uint8_t(ch), data, len) != 0) throw Error(lastError()); }
    /// Push queued unreliable messages; call once per frame.
    void flush() { aurix_net_flush(h_); }

    /// Dispatch pending events; call every frame on the game thread.
    int poll(int maxEvents = 256) {
        int n = 0; AurixNetEvent ev;
        while (n < maxEvents && aurix_net_poll(h_, &ev) == 1) {
            ++n;
            switch (ev.kind) {
                case AURIX_NET_EVENT_CONTROL: if (onControl) onControl(ev.json ? ev.json : ""); break;
                case AURIX_NET_EVENT_DATA: if (onData) onData(Channel(ev.channel), ev.tick, ev.data, ev.data_len); break;
                case AURIX_NET_EVENT_DATA_READY: if (onDataReady) onDataReady(); break;
                case AURIX_NET_EVENT_CLOSED: if (onClosed) onClosed(ev.json ? ev.json : ""); break;
                default: break;
            }
            aurix_net_event_free(&ev);
        }
        return n;
    }

    void leave() { if (h_) aurix_net_leave(h_); }
    void destroy() { if (h_) { aurix_net_destroy(h_); h_ = nullptr; actor_ = -1; } }

    static std::string quote(const std::string& s) {
        std::string o = "\"";
        for (unsigned char c : s) {
            if (c == '"' || c == '\\') { o += '\\'; o += char(c); }
            else if (c < 0x20) { char b[8]; snprintf(b, sizeof b, "\\u%04x", c); o += b; }
            else o += char(c);
        }
        return o + "\"";
    }
    static const char* modeName(Mode m) { return m == Mode::Relay ? "relay" : m == Mode::Sync ? "sync" : "lockstep"; }

private:
    AurixNetClient* h_ = nullptr; int actor_ = -1;
};

} // namespace aurix::net
