// Header-only C++17 wrapper over libaurix_net (aurix_net.h). Used by the Godot SDK.
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

// Relay event frame: u8 code, u8 flags, u16 from (BE), [u16 group], [u16 n, n*u16], payload.
struct EventFrame {
    uint8_t code = 0; uint16_t from = 0; bool hasGroup = false; uint16_t group = 0;
    const uint8_t* payload = nullptr; size_t len = 0;
    static bool decode(const uint8_t* b, size_t n, EventFrame& f) {
        if (!b || n < 4) return false;
        f.code = b[0]; uint8_t flags = b[1]; f.from = uint16_t((b[2] << 8) | b[3]); size_t o = 4;
        if (flags & 0x10) { if (n < o + 2) return false; f.hasGroup = true; f.group = (uint16_t)((b[o] << 8) | b[o + 1]); o += 2; }
        if ((flags & 3) == 3) { if (n < o + 2) return false; size_t c = (b[o] << 8) | b[o + 1]; o += 2 + c * 2; if (n < o) return false; }
        f.payload = b + o; f.len = n - o; return true;
    }
};

class Client {
public:
    std::function<void(const std::string& json)> onControl;
    /// System notice from the operator or game backend: (text, level = "info" | "warn").
    std::function<void(const std::string& text, const std::string& level)> onNotice;
    /// The room moved to another node: rejoin with the same grant (see fallback URLs).
    std::function<void()> onRehost;
    // Minimal extraction of a top-level string field from a control message (no JSON dependency).
    static std::string jsonStr(const std::string& j, const char* key) {
        std::string k = std::string("\"") + key + "\":";
        size_t p = j.find(k);
        if (p == std::string::npos) return "";
        p += k.size();
        while (p < j.size() && j[p] == ' ') ++p;
        if (p >= j.size() || j[p] != '"') return "";
        std::string out;
        for (++p; p < j.size() && j[p] != '"'; ++p) {
            if (j[p] == '\\' && p + 1 < j.size()) { ++p; out += (j[p] == 'n' ? '\n' : j[p]); }
            else out += j[p];
        }
        return out;
    }

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
    AurixNetClient* handle() const { return h_; }
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
                case AURIX_NET_EVENT_CONTROL: {
                    std::string j = ev.json ? ev.json : "";
                    if (onControl) onControl(j);
                    std::string t = jsonStr(j, "t");
                    if (t == "notice" && onNotice) onNotice(jsonStr(j, "text"), jsonStr(j, "level"));
                    if (t == "left" && onRehost && jsonStr(j, "reason") == "rehost") onRehost();
                    break;
                }
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

/// Lockstep room helper: feed every Data payload to receive(), step your deterministic simulation
/// on each Tick, answer Restore/CheckpointReq/DumpReq. Input delay adapts to the node's margin reports.
class Lockstep {
public:
    enum class Kind { None = 0, Tick = 1, Restore = 2, CheckpointReq = 3, DumpReq = 4 };
    struct Input { uint16_t actor; const uint8_t* data; size_t len; };
    struct Event { Kind kind; uint32_t tick; uint64_t hash; std::vector<Input> inputs; const uint8_t* state; size_t stateLen; };

    Lockstep() : ls_(aurix_net_lockstep_new()) {}
    ~Lockstep() { aurix_net_lockstep_destroy(ls_); }
    Lockstep(const Lockstep&) = delete; Lockstep& operator=(const Lockstep&) = delete;

    /// True when the payload was a lockstep frame (events may now be pending in poll()).
    bool receive(const uint8_t* data, size_t len) { return aurix_net_lockstep_receive(ls_, data, len) == 1; }
    /// Next event; Event::kind == Kind::None when there is nothing. Pointers stay valid until the next poll().
    Event poll() {
        AurixNetLsEvent e{}; Event out{Kind::None, 0, 0, {}, nullptr, 0};
        if (aurix_net_lockstep_poll(ls_, &e) != 1) return out;
        out.kind = Kind(e.kind); out.tick = e.tick; out.hash = e.hash; out.state = e.state; out.stateLen = e.state_len;
        for (uint16_t i = 0; i < e.inputs; i++) { Input in{}; if (aurix_net_lockstep_input(ls_, i, &in.actor, &in.data, &in.len) == 1) out.inputs.push_back(in); }
        return out;
    }
    /// Sends this tick's local input through the client; returns the tick it was stamped with (or -1).
    int64_t sendInput(Client& c, const uint8_t* data, size_t len) { return aurix_net_lockstep_send_input(ls_, c.handle(), data, len); }
    static void reportHash(Client& c, uint32_t tick, uint64_t hash) { aurix_net_lockstep_report_hash(c.handle(), tick, hash); }
    static void sendCheckpoint(Client& c, uint32_t tick, uint64_t hash, const uint8_t* s, size_t n) { aurix_net_lockstep_send_checkpoint(c.handle(), tick, hash, s, n); }
    static void sendDump(Client& c, uint32_t tick, uint64_t hash, const uint8_t* s, size_t n) { aurix_net_lockstep_send_dump(c.handle(), tick, hash, s, n); }
    static uint64_t fnv1a(const uint8_t* s, size_t n) { return aurix_net_lockstep_fnv1a(s, n); }

    int64_t get(int32_t key) const { return aurix_net_lockstep_get(ls_, key); }
    void set(int32_t key, int64_t v) { aurix_net_lockstep_set(ls_, key, v); }
    uint32_t inputDelay() const { return uint32_t(get(AURIX_NET_LS_INPUT_DELAY)); }
    void setInputDelay(uint32_t d, uint32_t min = 1, uint32_t max = 8, bool adaptive = true) {
        set(AURIX_NET_LS_INPUT_DELAY, d); set(AURIX_NET_LS_INPUT_DELAY_MIN, min); set(AURIX_NET_LS_INPUT_DELAY_MAX, max); set(AURIX_NET_LS_ADAPTIVE_DELAY, adaptive ? 1 : 0);
    }
    void setInputRedundancy(uint32_t n) { set(AURIX_NET_LS_INPUT_REDUNDANCY, n); }
    uint32_t confirmedTick() const { return uint32_t(get(AURIX_NET_LS_CONFIRMED_TICK)); }
    uint32_t serverTick() const { return uint32_t(get(AURIX_NET_LS_SERVER_TICK)); }
    int lastMargin() const { return int(get(AURIX_NET_LS_LAST_MARGIN)); }
    int64_t lateInputs() const { return get(AURIX_NET_LS_LATE_INPUTS); }

private:
    AurixNetLockstep* ls_;
};

/// Sync room helper (mirrors the Web `SyncSession`): feed every Data payload to receive() — it applies
/// the delta and sends the ack — then poll() spawn/change/despawn, read fields by name, and send raw
/// inputs (rooms with `logic`) or owner-authoritative component writes. renderTick() lags the server by
/// `interpolationTicks` for smoothing remote entities.
class Sync {
public:
    enum class Kind { None = 0, Spawn = 1, Change = 2, Despawn = 3 };
    struct Event { Kind kind; uint64_t entity; int owner; };

    explicit Sync(const std::string& schema, int tickHz = 60, int interpolationTicks = 2)
        : s_(aurix_net_sync_new(schema.c_str())), tickHz_(tickHz), interp_(interpolationTicks) {
        if (!s_) throw Error(lastError());
    }
    ~Sync() { aurix_net_sync_destroy(s_); }
    Sync(const Sync&) = delete; Sync& operator=(const Sync&) = delete;

    /// True when the payload was a delta: it is applied, the ack goes out through `c`, events wait in poll().
    bool receive(Client& c, const uint8_t* d, size_t n) {
        uint8_t ack[9];
        int r = aurix_net_sync_receive(s_, d, n, ack);
        if (r < 0) throw Error(lastError());
        if (r != 1) return false;
        c.sendData(Channel::Unreliable, ack, sizeof ack);
        double lag = double(serverTick()) - interp_;
        if (lag > renderTick_) renderTick_ = lag;
        gotDelta_ = true;
        return true;
    }
    Event poll() {
        AurixNetSyncEvent e{}; Event out{Kind::None, 0, -1};
        if (aurix_net_sync_poll(s_, &e) == 1) { out.kind = Kind(e.kind); out.entity = e.entity; out.owner = e.owner; }
        return out;
    }
    uint32_t serverTick() const { return aurix_net_sync_server_tick(s_); }
    uint64_t gaps() const { return aurix_net_sync_gaps(s_); }
    /// Call once per frame; returns the render tick (fractional, `interpolationTicks` behind the server).
    double advance(double dtSeconds) { if (gotDelta_) renderTick_ += dtSeconds * tickHz_; return renderTick_; }
    double renderTick() const { return renderTick_; }

    int entityCount() const { return aurix_net_sync_entity_count(s_); }
    bool entityAt(int index, uint64_t& id, int& owner) const {
        int32_t o = -1; int r = aurix_net_sync_entity_at(s_, index, &id, &o); owner = o; return r == 1;
    }
    int component(const char* name) const { return aurix_net_sync_component(s_, name); }
    int field(int component, const char* name) const { return aurix_net_sync_field(s_, component, name); }
    bool has(uint64_t entity, int component) const { return aurix_net_sync_has(s_, entity, component) == 1; }
    bool number(uint64_t entity, int component, int field, double& out) const { return aurix_net_sync_get_number(s_, entity, component, field, &out) == 1; }
    /// Fills out[0..n) and returns n (2 vec2, 3 vec3, 4 quat x,y,z,w, 1 scalar), 0 when missing.
    int vector(uint64_t entity, int component, int field, float out[4]) const { return aurix_net_sync_get_vector(s_, entity, component, field, out); }
    std::string text(uint64_t entity, int component, int field) const {
        int n = aurix_net_sync_get_bytes(s_, entity, component, field, nullptr, 0);
        if (n <= 0) return {};
        std::string s(size_t(n), '\0');
        aurix_net_sync_get_bytes(s_, entity, component, field, reinterpret_cast<uint8_t*>(&s[0]), s.size());
        return s;
    }

    /// Raw input for server-side logic; returns the stamped tick (resent with the next two inputs).
    int64_t sendInput(Client& c, const uint8_t* d, size_t n) { return aurix_net_sync_send_input(s_, c.handle(), d, n); }
    /// Owner-authoritative writes: begin(component).write(field, …)….end(), then sendWrites().
    Sync& begin(int component) { check(aurix_net_sync_write_begin(s_, component)); return *this; }
    Sync& write(int field, double v) { check(aurix_net_sync_write_number(s_, field, v)); return *this; }
    Sync& write(int field, const float* v, int n) { check(aurix_net_sync_write_vector(s_, field, v, n)); return *this; }
    Sync& write(int field, const std::string& s) { check(aurix_net_sync_write_bytes(s_, field, reinterpret_cast<const uint8_t*>(s.data()), s.size())); return *this; }
    Sync& write(int field, const uint8_t* p, size_t n) { check(aurix_net_sync_write_bytes(s_, field, p, n)); return *this; }
    Sync& end() { check(aurix_net_sync_write_end(s_)); return *this; }
    int64_t sendWrites(Client& c) { return aurix_net_sync_send_writes(s_, c.handle()); }

private:
    static void check(int r) { if (r != 0) throw Error(lastError()); }
    AurixNetSync* s_; int tickHz_; double interp_; double renderTick_ = 0; bool gotDelta_ = false;
};

} // namespace aurix::net
