// Header-only C++17 wrapper over libaurix_net (aurix_net.h). Used by the Godot SDK.
#pragma once
#include <cstdint>
#include <functional>
#include <optional>
#include <set>
#include <stdexcept>
#include <string>
#include <vector>

#include "aurix_net.h"

namespace aurix::net {

/// Unreliable drops a message that arrives after a newer one; UnreliableUnsequenced delivers every
/// message that arrives, in the order of arrival.
enum class Channel : uint8_t {
    Unreliable = 0,
    Reliable = 1,
    ReliableUnordered = 2,
    UnreliableUnsequenced = 4
};
enum class Target : int32_t { All = 0, Others = 1, Master = 2, Actors = 3 };
enum class Mode { Relay, Sync, Lockstep };

inline std::string lastError() {
    const char* error = aurix_net_last_error();
    return error ? error : "unknown error";
}

struct Error : std::runtime_error {
    /// One of AURIX_NET_ERR_*: why the call failed, without reading the text.
    int code;
    Error(const std::string& message, int code) : std::runtime_error(message), code(code) {}
    /// The failure the C interface reported last on this thread.
    static Error last() { return Error(lastError(), aurix_net_last_error_code()); }
};

// Relay event frame: u8 code, u8 flags, u16 from (BE), [u16 group], [u16 n, n*u16], payload.
struct EventFrame {
    uint8_t code = 0;
    uint16_t from = 0;
    bool hasGroup = false;
    uint16_t group = 0;
    const uint8_t* payload = nullptr;
    size_t len = 0;
    static bool decode(const uint8_t* bytes, size_t len, EventFrame& frame) {
        if (!bytes || len < 4) {
            return false;
        }
        frame.code = bytes[0];
        uint8_t flags = bytes[1];
        frame.from = uint16_t((bytes[2] << 8) | bytes[3]);
        size_t offset = 4;
        if (flags & 0x10) {
            if (len < offset + 2) {
                return false;
            }
            frame.hasGroup = true;
            frame.group = (uint16_t)((bytes[offset] << 8) | bytes[offset + 1]);
            offset += 2;
        }
        if ((flags & 3) == 3) {
            if (len < offset + 2) {
                return false;
            }
            size_t count = (bytes[offset] << 8) | bytes[offset + 1];
            offset += 2 + count * 2;
            if (len < offset) {
                return false;
            }
        }
        frame.payload = bytes + offset;
        frame.len = len - offset;
        return true;
    }
};

class Client {
public:
    std::function<void(const std::string& json)> onControl;
    /// System notice from the operator or game backend: (text, level = "info" | "warn").
    std::function<void(const std::string& text, const std::string& level)> onNotice;
    /// The room moved to another node: rejoin with the same grant (see fallback URLs).
    std::function<void()> onRehost;
    /// A top-level string field of a control message; empty when absent.
    static std::string jsonStr(const std::string& json, const char* key) {
        char* value = aurix_net_json_string(json.c_str(), key);
        if (!value) {
            return "";
        }
        std::string out(value);
        aurix_net_string_free(value);
        return out;
    }

    std::function<void(Channel, uint16_t tick, const uint8_t*, size_t)> onData;
    std::function<void()> onDataReady;
    std::function<void(const std::string& reason)> onClosed;

    Client() = default;
    Client(const Client&) = delete;
    Client& operator=(const Client&) = delete;
    Client(Client&& other) noexcept
        : onControl(std::move(other.onControl)),
          onNotice(std::move(other.onNotice)),
          onRehost(std::move(other.onRehost)),
          onData(std::move(other.onData)),
          onDataReady(std::move(other.onDataReady)),
          onClosed(std::move(other.onClosed)),
          handle_(other.handle_),
          actor_(other.actor_) {
        other.handle_ = nullptr;
        other.actor_ = -1;
    }
    ~Client() { destroy(); }

    static std::string version() { return aurix_net_version(); }
    /// Backend/tooling only.
    static std::string issueGrant(const std::string& http, const std::string& apiKey,
                                  const std::string& player, const std::string& name) {
        char* grant =
            aurix_net_issue_grant(http.c_str(), apiKey.c_str(), player.c_str(), name.c_str());
        if (!grant) {
            throw Error::last();
        }
        std::string result(grant);
        aurix_net_string_free(grant);
        return result;
    }

    void connect(const std::string& http, const std::string& grant, bool tunnel = false) {
        destroy();
        handle_ = aurix_net_connect(http.c_str(), grant.c_str(), tunnel ? 1 : 0);
        if (!handle_) {
            throw Error::last();
        }
    }
    /// connect + join in one call (`json` is the join message, as for `joinJson`). When the room
    /// lives on another node of the region, the client ends up connected to that node.
    int connectAndJoin(const std::string& http, const std::string& grant, const std::string& json,
                       bool tunnel = false) {
        destroy();
        int actor = -1;
        handle_ = aurix_net_connect_and_join(http.c_str(), grant.c_str(), tunnel ? 1 : 0,
                                             json.c_str(), &actor);
        if (!handle_) {
            throw Error::last();
        }
        actor_ = actor;
        return actor;
    }
    bool connected() const { return handle_ != nullptr; }
    AurixNetClient* handle() const { return handle_; }
    int actor() const { return actor_; }

    /// Region list and best-region ping: JSON {region, endpoint, rtt_ms, candidates} from the
    /// catalog or any node.
    static std::string bestRegionJson(const std::string& catalogHttp, int probes = 3) {
        char* json = aurix_net_best_region(catalogHttp.c_str(), probes);
        if (!json) {
            throw Error::last();
        }
        std::string out(json);
        aurix_net_string_free(json);
        return out;
    }
    /// The fastest node's HTTP URL (use it as the cell URL for connect).
    static std::string bestRegionEndpoint(const std::string& catalogHttp, int probes = 3) {
        return jsonStr(bestRegionJson(catalogHttp, probes), "endpoint");
    }
    /// Protocol JSON, e.g. {"t":"set_room_props","props":{...}}.
    void sendControl(const std::string& json) {
        if (aurix_net_send_control(handle_, json.c_str()) != 0) {
            throw Error::last();
        }
    }
    /// Sync rooms: which entities define this player's viewpoints (AOI centres, at most 4) and
    /// which extra entities are always of interest (explicit interest). Empty = defaults (own
    /// avatar).
    void setView(const std::vector<uint64_t>& viewpoints,
                 const std::vector<uint64_t>& interest = {}) {
        sendControl("{\"t\":\"set_view\",\"viewpoints\":" + joinU64(viewpoints) +
                    ",\"interest\":" + joinU64(interest) + "}");
    }
    /// Relay interest groups: join and leave group ids (events raised to a group reach only its
    /// members).
    void setGroups(const std::vector<uint16_t>& join, const std::vector<uint16_t>& leave = {}) {
        std::vector<uint64_t> joined(join.begin(), join.end()), left(leave.begin(), leave.end());
        sendControl("{\"t\":\"set_groups\",\"join\":" + joinU64(joined) +
                    ",\"leave\":" + joinU64(left) + "}");
    }

private:
    static std::string joinU64(const std::vector<uint64_t>& ids) {
        std::string out = "[";
        for (size_t i = 0; i < ids.size(); ++i) {
            if (i) {
                out += ",";
            }
            out += std::to_string(ids[i]);
        }
        return out + "]";
    }
    static std::string quoted(const std::vector<std::string>& texts) {
        std::string out = "[";
        for (size_t i = 0; i < texts.size(); ++i) {
            out += (i ? "," : "") + quote(texts[i]);
        }
        return out + "]";
    }

public:
    /// `lobbyProps` names the room props that room listings show and `expr` filters match.
    /// `reserved`: player ids whose seats are held for 60 s when this call creates the room (the
    /// `players` of a matchmaking ticket).
    int joinOrCreate(const std::string& name, Mode mode = Mode::Relay, int maxPlayers = 8,
                     const std::string& propsJson = "",
                     const std::vector<std::string>& lobbyProps = {},
                     const std::vector<std::string>& reserved = {}) {
        std::string message = "{\"t\":\"join_or_create\",\"name\":" + quote(name) + ",\"mode\":\"" +
                              modeName(mode) + "\",\"max_players\":" + std::to_string(maxPlayers);
        if (!propsJson.empty()) {
            message += ",\"props\":" + propsJson;
        }
        if (!lobbyProps.empty()) {
            message += ",\"lobby_props\":" + quoted(lobbyProps);
        }
        if (!reserved.empty()) {
            message += ",\"reserved\":" + quoted(reserved);
        }
        return joinJson(message + "}");
    }
    /// Joins a random open room. `filterJson` must equal room props exactly, `expr` is a SQL-like
    /// condition on lobby props (`map = 'dust' AND skill <= 1500`), `matchType` is `fill`
    /// (default), `serial` or `random`.
    int joinRandom(const std::string& filterJson = "", const std::string& expr = "",
                   const std::string& matchType = "") {
        std::string message = "{\"t\":\"join_random\"";
        if (!filterJson.empty()) {
            message += ",\"filter\":" + filterJson;
        }
        if (!expr.empty()) {
            message += ",\"expr\":" + quote(expr);
        }
        if (!matchType.empty()) {
            message += ",\"match_type\":" + quote(matchType);
        }
        return joinJson(message + "}");
    }
    int join(const std::string& name, const std::string& schemaHash = "") {
        std::string message = "{\"t\":\"join\",\"name\":" + quote(name);
        if (!schemaHash.empty()) {
            message += ",\"schema_hash\":" + quote(schemaHash);
        }
        return joinJson(message + "}");
    }
    int joinJson(const std::string& json) {
        int actor = aurix_net_join(handle_, json.c_str());
        if (actor < 0) {
            throw Error::last();
        }
        actor_ = actor;
        return actor;
    }
    std::string joinedJson() const {
        char* joined = aurix_net_joined_json(handle_);
        if (!joined) {
            return "";
        }
        std::string result(joined);
        aurix_net_string_free(joined);
        return result;
    }
    /// The seed of the lockstep room: every client and the node start the game from it.
    std::optional<uint64_t> seed() const {
        uint64_t seed = 0;
        if (aurix_net_joined_seed(handle_, &seed) != 0) {
            return std::nullopt;
        }
        return seed;
    }
    /// The entity a sync room made for this client as its avatar, once the room has announced
    /// it. Kept from the join on, so it answers whenever the game asks.
    std::optional<uint64_t> avatar() const {
        uint64_t entity = 0;
        if (aurix_net_avatar(handle_, &entity) != 1) {
            return std::nullopt;
        }
        return entity;
    }

    void sendEvent(uint8_t code, const uint8_t* data, size_t len, Target target = Target::Others,
                   bool reliable = false, const std::vector<uint16_t>& actors = {}) {
        if (aurix_net_send_event(handle_, code, int32_t(target),
                                 actors.empty() ? nullptr : actors.data(), actors.size(), data, len,
                                 reliable ? 1 : 0) != 0) {
            throw Error::last();
        }
    }
    /// sendEvent plus interest group (-1 = none) and cache (0 none, 1 add for late joiners, 2
    /// remove).
    void sendEventEx(uint8_t code, const uint8_t* data, size_t len, Target target, bool reliable,
                     const std::vector<uint16_t>& actors, int group, int cache) {
        if (aurix_net_send_event_ex(handle_, code, (int32_t)target,
                                    actors.empty() ? nullptr : actors.data(), actors.size(), data,
                                    len, reliable ? 1 : 0, group, cache) != 0) {
            throw Error::last();
        }
    }
    /// sendEvent on a channel of the caller's choice: Channel::Unreliable for state, where a newer
    /// event replaces an older one, Channel::UnreliableUnsequenced for events that are all wanted.
    /// A node that does not take the unsequenced channel gets them on the sequenced one.
    void sendEventOn(Channel channel, uint8_t code, const uint8_t* data, size_t len,
                     Target target = Target::Others, const std::vector<uint16_t>& actors = {},
                     int group = -1, int cache = 0) {
        if (aurix_net_send_event_on(handle_, uint8_t(channel), code, int32_t(target),
                                    actors.empty() ? nullptr : actors.data(), actors.size(), data,
                                    len, group, cache) != 0) {
            throw Error::last();
        }
    }
    void sendData(Channel ch, const uint8_t* data, size_t len) {
        if (aurix_net_send_data(handle_, uint8_t(ch), data, len) != 0) {
            throw Error::last();
        }
    }
    /// Push queued unreliable messages; call once per frame.
    void flush() { aurix_net_flush(handle_); }

    /// Dispatch pending events; call every frame on the game thread.
    int poll(int maxEvents = 256) {
        int count = 0;
        AurixNetEvent ev;
        while (count < maxEvents && aurix_net_poll(handle_, &ev) == 1) {
            ++count;
            switch (ev.kind) {
                case AURIX_NET_EVENT_CONTROL: {
                    std::string message = ev.json ? ev.json : "";
                    std::string type = jsonStr(message, "t");
                    if (type == "left" && joinedJson().empty()) {
                        actor_ = -1;
                    }
                    if (onControl) {
                        onControl(message);
                    }
                    if (type == "notice" && onNotice) {
                        onNotice(jsonStr(message, "text"), jsonStr(message, "level"));
                    }
                    if (type == "left" && onRehost && jsonStr(message, "reason") == "rehost") {
                        onRehost();
                    }
                    break;
                }
                case AURIX_NET_EVENT_DATA:
                    if (onData) {
                        onData(Channel(ev.channel), ev.tick, ev.data, ev.data_len);
                    }
                    break;
                case AURIX_NET_EVENT_DATA_READY:
                    if (onDataReady) {
                        onDataReady();
                    }
                    break;
                case AURIX_NET_EVENT_CLOSED:
                    if (onClosed) {
                        onClosed(ev.json ? ev.json : "");
                    }
                    break;
                default:
                    break;
            }
            aurix_net_event_free(&ev);
        }
        return count;
    }

    void leave() {
        if (handle_) {
            aurix_net_leave(handle_);
        }
        actor_ = -1;
    }
    void destroy() {
        if (handle_) {
            aurix_net_destroy(handle_);
            handle_ = nullptr;
            actor_ = -1;
        }
    }

    static std::string quote(const std::string& text) {
        std::string quoted = "\"";
        for (unsigned char ch : text) {
            if (ch == '"' || ch == '\\') {
                quoted += '\\';
                quoted += char(ch);
            } else if (ch < 0x20) {
                char escaped[8];
                snprintf(escaped, sizeof escaped, "\\u%04x", ch);
                quoted += escaped;
            } else {
                quoted += char(ch);
            }
        }
        return quoted + "\"";
    }
    static const char* modeName(Mode mode) {
        return mode == Mode::Relay ? "relay" : mode == Mode::Sync ? "sync" : "lockstep";
    }

private:
    AurixNetClient* handle_ = nullptr;
    int actor_ = -1;
};

/// Lockstep room helper: feed every Data payload to receive(), step your deterministic simulation
/// on each Tick, answer Restore/CheckpointReq/DumpReq. Input delay adapts to the node's margin
/// reports.
class Lockstep {
public:
    enum class Kind { None = 0, Tick = 1, Restore = 2, CheckpointReq = 3, DumpReq = 4 };
    struct Input {
        uint16_t actor;
        const uint8_t* data;
        size_t len;
    };
    struct Event {
        Kind kind;
        uint32_t tick;
        uint64_t hash;
        std::vector<Input> inputs;
        const uint8_t* state;
        size_t stateLen;
    };

    Lockstep() : lockstep_(aurix_net_lockstep_new()) {}
    ~Lockstep() { aurix_net_lockstep_destroy(lockstep_); }
    Lockstep(const Lockstep&) = delete;
    Lockstep& operator=(const Lockstep&) = delete;

    /// True when the payload was a lockstep frame (events may now be pending in poll()). Bundles
    /// are acknowledged through `client`, so the room resends the ones that were lost.
    bool receive(Client& client, const uint8_t* data, size_t len) {
        return aurix_net_lockstep_receive(lockstep_, client.handle(), data, len) == 1;
    }
    /// Next event; Event::kind == Kind::None when there is nothing. Pointers stay valid until the
    /// next poll().
    Event poll() {
        AurixNetLsEvent event{};
        Event out{Kind::None, 0, 0, {}, nullptr, 0};
        if (aurix_net_lockstep_poll(lockstep_, &event) != 1) {
            return out;
        }
        out.kind = Kind(event.kind);
        out.tick = event.tick;
        out.hash = event.hash;
        out.state = event.state;
        out.stateLen = event.state_len;
        for (uint16_t i = 0; i < event.inputs; i++) {
            Input in{};
            if (aurix_net_lockstep_input(lockstep_, i, &in.actor, &in.data, &in.len) == 1) {
                out.inputs.push_back(in);
            }
        }
        return out;
    }
    /// Sends this tick's local input through the client; returns the tick it was stamped with (or
    /// -1).
    int64_t sendInput(Client& client, const uint8_t* data, size_t len) {
        return aurix_net_lockstep_send_input(lockstep_, client.handle(), data, len);
    }
    /// Reliable command outside the input stream; everyone gets it next tick as input of actor
    /// AURIX_NET_COMMAND_ACTOR.
    void sendCommand(Client& client, const std::vector<uint8_t>& data) {
        if (aurix_net_lockstep_send_command(client.handle(), data.data(), data.size()) != 0) {
            throw Error::last();
        }
    }
    static void reportHash(Client& client, uint32_t tick, uint64_t hash) {
        aurix_net_lockstep_report_hash(client.handle(), tick, hash);
    }
    static void sendCheckpoint(Client& client, uint32_t tick, uint64_t hash, const uint8_t* data,
                               size_t len) {
        aurix_net_lockstep_send_checkpoint(client.handle(), tick, hash, data, len);
    }
    static void sendDump(Client& client, uint32_t tick, uint64_t hash, const uint8_t* data,
                         size_t len) {
        aurix_net_lockstep_send_dump(client.handle(), tick, hash, data, len);
    }
    static uint64_t fnv1a(const uint8_t* data, size_t len) {
        return aurix_net_lockstep_fnv1a(data, len);
    }

    int64_t get(int32_t key) const { return aurix_net_lockstep_get(lockstep_, key); }
    void set(int32_t key, int64_t value) { aurix_net_lockstep_set(lockstep_, key, value); }
    uint32_t inputDelay() const { return uint32_t(get(AURIX_NET_LS_INPUT_DELAY)); }
    void setInputDelay(uint32_t delay, uint32_t min = 1, uint32_t max = 8, bool adaptive = true) {
        set(AURIX_NET_LS_INPUT_DELAY, delay);
        set(AURIX_NET_LS_INPUT_DELAY_MIN, min);
        set(AURIX_NET_LS_INPUT_DELAY_MAX, max);
        set(AURIX_NET_LS_ADAPTIVE_DELAY, adaptive ? 1 : 0);
    }
    void setInputRedundancy(uint32_t count) { set(AURIX_NET_LS_INPUT_REDUNDANCY, count); }
    uint32_t confirmedTick() const { return uint32_t(get(AURIX_NET_LS_CONFIRMED_TICK)); }
    uint32_t serverTick() const { return uint32_t(get(AURIX_NET_LS_SERVER_TICK)); }
    int lastMargin() const { return int(get(AURIX_NET_LS_LAST_MARGIN)); }
    int64_t lateInputs() const { return get(AURIX_NET_LS_LATE_INPUTS); }

private:
    AurixNetLockstep* lockstep_;
};

/// Sync room helper (mirrors the Web `SyncSession`): feed every Data payload to receive(), which
/// applies the delta and sends the ack, then poll() spawn/change/despawn, read fields by name, and
/// send raw inputs (rooms with `logic`) or owner-authoritative component writes. renderTick() lags
/// the server by `interpolationTicks` for smoothing remote entities.
class Sync {
public:
    enum class Kind { None = 0, Spawn = 1, Change = 2, Despawn = 3 };
    struct Event {
        Kind kind;
        uint64_t entity;
        int owner;
    };
    /// Refused: the actor's entity limit, or the room is full. Invalid: the room could not read
    /// the spawn.
    enum class SpawnStatus { Created = 0, Refused = 1, Invalid = 2 };
    /// The room's answer to spawn(): the entity it made for `tag`, or why it made none. `tag` 0
    /// names the avatar (Client::avatar()).
    struct SpawnAnswer {
        uint32_t tag;
        SpawnStatus status;
        uint64_t entity;
        uint32_t tick;
    };

    explicit Sync(const std::string& schema, int tickHz = 60, int interpolationTicks = 2)
        : sync_(aurix_net_sync_new(schema.c_str())), tickHz_(tickHz), interp_(interpolationTicks) {
        if (!sync_) {
            throw Error::last();
        }
    }
    ~Sync() { aurix_net_sync_destroy(sync_); }
    Sync(const Sync&) = delete;
    Sync& operator=(const Sync&) = delete;

    /// True when the payload was a sync frame. A delta is applied, its ack goes out through `c`
    /// and events wait in poll(); an input margin report moves the lead of the next inputs.
    bool receive(Client& client, const uint8_t* data, size_t len) {
        uint8_t ack[9];
        int result = aurix_net_sync_receive(sync_, data, len, ack);
        if (result < 0) {
            throw Error::last();
        }
        if (result != 1) {
            return result != 0;
        }
        client.sendData(Channel::Unreliable, ack, sizeof ack);
        aurix_net_sync_pump(sync_, client.handle());
        double lag = double(serverTick()) - interp_;
        if (lag > renderTick_) {
            renderTick_ = lag;
        }
        gotDelta_ = true;
        return true;
    }
    Event poll() {
        AurixNetSyncEvent event{};
        Event out{Kind::None, 0, -1};
        if (aurix_net_sync_poll(sync_, &event) == 1) {
            out.kind = Kind(event.kind);
            out.entity = event.entity;
            out.owner = event.owner;
        }
        return out;
    }
    /// The next answer to spawn(). It and the delta that carries the entity come in either order.
    std::optional<SpawnAnswer> pollSpawn() {
        AurixNetSyncSpawn answer{};
        if (aurix_net_sync_poll_spawn(sync_, &answer) != 1) {
            return std::nullopt;
        }
        return SpawnAnswer{answer.tag, SpawnStatus(answer.status), answer.entity, answer.tick};
    }
    uint32_t serverTick() const { return aurix_net_sync_server_tick(sync_); }
    uint64_t gaps() const { return aurix_net_sync_gaps(sync_); }
    /// Call once per frame; returns the render tick (fractional, `interpolationTicks` behind the
    /// server).
    double advance(double dtSeconds) {
        if (gotDelta_) {
            renderTick_ += dtSeconds * tickHz_;
        }
        return renderTick_;
    }
    double renderTick() const { return renderTick_; }

    int entityCount() const { return aurix_net_sync_entity_count(sync_); }
    bool entityAt(int index, uint64_t& id, int& owner) const {
        int32_t ownerId = -1;
        int result = aurix_net_sync_entity_at(sync_, index, &id, &ownerId);
        owner = ownerId;
        return result == 1;
    }
    int component(const char* name) const { return aurix_net_sync_component(sync_, name); }
    int field(int component, const char* name) const {
        return aurix_net_sync_field(sync_, component, name);
    }
    bool has(uint64_t entity, int component) const {
        return aurix_net_sync_has(sync_, entity, component) == 1;
    }
    bool number(uint64_t entity, int component, int field, double& out) const {
        return aurix_net_sync_get_number(sync_, entity, component, field, &out) == 1;
    }
    /// Fills out[0..n) and returns n (2 vec2, 3 vec3, 4 quat x,y,z,w, 1 scalar), 0 when missing.
    int vector(uint64_t entity, int component, int field, float out[4]) const {
        return aurix_net_sync_get_vector(sync_, entity, component, field, out);
    }
    std::string text(uint64_t entity, int component, int field) const {
        int len = aurix_net_sync_get_bytes(sync_, entity, component, field, nullptr, 0);
        if (len <= 0) {
            return {};
        }
        std::string text(size_t(len), '\0');
        aurix_net_sync_get_bytes(sync_, entity, component, field,
                                 reinterpret_cast<uint8_t*>(&text[0]), text.size());
        return text;
    }

    /// Raw input for server-side logic; returns the stamped tick (resent with the next two inputs).
    int64_t sendInput(Client& client, const uint8_t* data, size_t len) {
        return aurix_net_sync_send_input(sync_, client.handle(), data, len);
    }
    /// Owner-authoritative writes: begin(component).write(field, …)….end(), then sendWrites().
    Sync& begin(int component) {
        check(aurix_net_sync_write_begin(sync_, component));
        return *this;
    }
    Sync& write(int field, double value) {
        check(aurix_net_sync_write_number(sync_, field, value));
        return *this;
    }
    Sync& write(int field, const float* values, int count) {
        check(aurix_net_sync_write_vector(sync_, field, values, count));
        return *this;
    }
    Sync& write(int field, const std::string& text) {
        check(aurix_net_sync_write_bytes(
            sync_, field, reinterpret_cast<const uint8_t*>(text.data()), text.size()));
        return *this;
    }
    Sync& write(int field, const uint8_t* data, size_t len) {
        check(aurix_net_sync_write_bytes(sync_, field, data, len));
        return *this;
    }
    Sync& end() {
        check(aurix_net_sync_write_end(sync_));
        return *this;
    }
    int64_t sendWrites(Client& client) {
        return aurix_net_sync_send_writes(sync_, client.handle());
    }
    /// Owner-authoritative ops on entities you own (queued writes feed write/spawn): spawn, despawn
    /// and ownership hand-off.
    int64_t writeEntity(Client& client, uint64_t entity) {
        return aurix_net_sync_send_op(sync_, client.handle(), 0, entity, 0);
    }
    /// Returns the tag the room answers with (pollSpawn()), or -1 when the room does not answer
    /// tagged spawns. The spawn is repeated until the room answers.
    int64_t spawn(Client& client) { return aurix_net_sync_spawn(sync_, client.handle()); }
    int64_t despawn(Client& client, uint64_t entity) {
        return aurix_net_sync_send_op(sync_, client.handle(), 2, entity, 0);
    }
    int64_t transfer(Client& client, uint64_t entity, int toActor) {
        return aurix_net_sync_send_op(sync_, client.handle(), 3, entity, toActor);
    }
    /// Ownership policy: 0 destroy_on_leave (default), 1 to_master, 2 orphan, 3 dynamic. Owner
    /// only.
    int64_t setPolicy(Client& client, uint64_t entity, int policy) {
        return aurix_net_sync_send_op(sync_, client.handle(), 4, entity, policy);
    }
    /// Take an unowned entity, or any entity with the dynamic policy; the cell arbitrates (owner
    /// unchanged = refused).
    int64_t requestOwnership(Client& client, uint64_t entity) {
        return aurix_net_sync_send_op(sync_, client.handle(), 5, entity, 0);
    }
    /// Drop ownership; the entity stays alive without an owner.
    int64_t release(Client& client, uint64_t entity) {
        return aurix_net_sync_send_op(sync_, client.handle(), 6, entity, 0);
    }

private:
    static void check(int result) {
        if (result != 0) {
            throw Error::last();
        }
    }
    AurixNetSync* sync_;
    int tickHz_;
    double interp_;
    double renderTick_ = 0;
    bool gotDelta_ = false;
};

/// Chat on top of a relay room: channels are interest groups (join = subscribe, groups at groupBase
/// + channel), messages are reliable cached events to the group (late joiners get the history the
/// room still holds once they join the channel), whispers are events to one actor. Feed every
/// received event frame to `onEventFrame` (from your onData handler) and read messages from
/// `onMessage`.
class Chat {
public:
    struct Message {
        uint16_t from = 0;
        bool whisper = false;
        uint16_t channel = 0;
        std::string text;
    };
    std::function<void(const Message&)> onMessage;
    explicit Chat(Client& client, uint8_t code = 240, uint16_t groupBase = 60000,
                  bool history = true)
        : client_(client), code_(code), base_(groupBase), history_(history) {}
    void join(uint16_t channel) {
        if (joined_.insert(channel).second) {
            client_.setGroups({(uint16_t)(base_ + channel)});
            std::vector<Message> held;
            held.swap(held_);
            for (auto& message : held) {
                if (message.channel != channel) {
                    held_.push_back(std::move(message));
                } else if (onMessage) {
                    onMessage(message);
                }
            }
        }
    }
    void leave(uint16_t channel) {
        if (joined_.erase(channel)) {
            client_.setGroups({}, {(uint16_t)(base_ + channel)});
        }
    }
    void send(uint16_t channel, const std::string& text) {
        auto bytes = encode(0, channel, text);
        client_.sendEventEx(code_, bytes.data(), bytes.size(), Target::All, true, {},
                            base_ + channel, history_ ? 1 : 0);
    }
    void whisper(uint16_t actor, const std::string& text) {
        auto bytes = encode(1, 0, text);
        client_.sendEventEx(code_, bytes.data(), bytes.size(), Target::Actors, true, {actor}, -1,
                            0);
    }
    /// Returns true when the frame was a chat message (consumed).
    bool onEventFrame(const EventFrame& frame) {
        if (frame.code != code_ || frame.len < 3) {
            return false;
        }
        Message message;
        message.from = frame.from;
        message.whisper = frame.payload[0] == 1;
        message.channel = (uint16_t)((frame.payload[1] << 8) | frame.payload[2]);
        message.text.assign((const char*)frame.payload + 3, frame.len - 3);
        if (!message.whisper && !joined_.count(message.channel)) {
            // Only the history replayed at the room join arrives for a channel not joined yet.
            held_.push_back(std::move(message));
            return true;
        }
        if (onMessage) {
            onMessage(message);
        }
        return true;
    }

private:
    static std::vector<uint8_t> encode(uint8_t kind, uint16_t channel, const std::string& text) {
        std::vector<uint8_t> bytes;
        bytes.reserve(3 + text.size());
        bytes.push_back(kind);
        bytes.push_back((uint8_t)(channel >> 8));
        bytes.push_back((uint8_t)channel);
        size_t len = text.size() > 2000 ? 2000 : text.size();
        bytes.insert(bytes.end(), text.begin(), text.begin() + len);
        return bytes;
    }
    Client& client_;
    uint8_t code_;
    uint16_t base_;
    bool history_;
    std::set<uint16_t> joined_;
    std::vector<Message> held_;
};
}  // namespace aurix::net
