#include "aurix_net_client.h"
#include <godot_cpp/core/class_db.hpp>

using namespace godot;

static std::string to_std(const String &s) { return std::string(s.utf8().get_data()); }
static PackedByteArray to_pba(const uint8_t *d, size_t n) { PackedByteArray p; p.resize(n); if (n) memcpy(p.ptrw(), d, n); return p; }

void AurixNetClient::_bind_methods() {
    ClassDB::bind_method(D_METHOD("connect_to", "http_url", "grant", "force_tunnel"), &AurixNetClient::connect_to, DEFVAL(false));
    ClassDB::bind_method(D_METHOD("join_or_create", "room", "mode", "max_players", "props_json"), &AurixNetClient::join_or_create, DEFVAL(MODE_RELAY), DEFVAL(8), DEFVAL(""));
    ClassDB::bind_method(D_METHOD("join", "room", "schema_hash"), &AurixNetClient::join, DEFVAL(""));
    ClassDB::bind_method(D_METHOD("send_control", "json"), &AurixNetClient::send_control);
    ClassDB::bind_method(D_METHOD("send_event", "code", "payload", "target", "reliable"), &AurixNetClient::send_event, DEFVAL(TARGET_OTHERS), DEFVAL(false));
    ClassDB::bind_method(D_METHOD("send_data", "channel", "payload"), &AurixNetClient::send_data);
    ClassDB::bind_method(D_METHOD("flush"), &AurixNetClient::flush);
    ClassDB::bind_method(D_METHOD("poll", "max_events"), &AurixNetClient::poll, DEFVAL(256));
    ClassDB::bind_method(D_METHOD("leave"), &AurixNetClient::leave);
    ClassDB::bind_method(D_METHOD("disconnect_from"), &AurixNetClient::disconnect_from);
    ClassDB::bind_method(D_METHOD("get_actor"), &AurixNetClient::get_actor);
    ClassDB::bind_method(D_METHOD("is_connected_to"), &AurixNetClient::is_connected_to);
    ClassDB::bind_method(D_METHOD("get_joined_json"), &AurixNetClient::get_joined_json);
    ClassDB::bind_method(D_METHOD("get_last_error"), &AurixNetClient::get_last_error);
    ADD_SIGNAL(MethodInfo("control", PropertyInfo(Variant::STRING, "json")));
    ADD_SIGNAL(MethodInfo("event", PropertyInfo(Variant::INT, "code"), PropertyInfo(Variant::INT, "from_actor"), PropertyInfo(Variant::PACKED_BYTE_ARRAY, "payload")));
    ADD_SIGNAL(MethodInfo("data", PropertyInfo(Variant::INT, "channel"), PropertyInfo(Variant::INT, "tick"), PropertyInfo(Variant::PACKED_BYTE_ARRAY, "payload")));
    ADD_SIGNAL(MethodInfo("closed", PropertyInfo(Variant::STRING, "reason")));
    ADD_SIGNAL(MethodInfo("notice", PropertyInfo(Variant::STRING, "text"), PropertyInfo(Variant::STRING, "level")));
    ADD_SIGNAL(MethodInfo("rehost"));
    BIND_ENUM_CONSTANT(MODE_RELAY); BIND_ENUM_CONSTANT(MODE_SYNC); BIND_ENUM_CONSTANT(MODE_LOCKSTEP);
    BIND_ENUM_CONSTANT(TARGET_ALL); BIND_ENUM_CONSTANT(TARGET_OTHERS); BIND_ENUM_CONSTANT(TARGET_MASTER);
}

void AurixNetClient::hook() {
    if (hooked_) return;
    hooked_ = true;
    client_.onControl = [this](const std::string &j) { emit_signal("control", String::utf8(j.c_str())); };
    client_.onClosed = [this](const std::string &r) { emit_signal("closed", String::utf8(r.c_str())); };
    client_.onNotice = [this](const std::string &text, const std::string &level) { emit_signal("notice", String::utf8(text.c_str()), String::utf8(level.c_str())); };
    client_.onRehost = [this]() { emit_signal("rehost"); };
    client_.onData = [this](aurix::net::Channel ch, uint16_t tick, const uint8_t *d, size_t n) {
        emit_signal("data", (int)ch, (int)tick, to_pba(d, n));
        aurix::net::EventFrame f;
        if (aurix::net::EventFrame::decode(d, n, f)) emit_signal("event", (int)f.code, (int)f.from, to_pba(f.payload, f.len));
    };
}

#define AURIX_TRY(expr, fail) try { expr; } catch (const std::exception &e) { last_error_ = e.what(); return fail; }

bool AurixNetClient::connect_to(const String &http_url, const String &grant, bool force_tunnel) {
    AURIX_TRY(client_.connect(to_std(http_url), to_std(grant), force_tunnel); hook(); return true, false);
}
int AurixNetClient::join_or_create(const String &room, int mode, int max_players, const String &props_json) {
    AURIX_TRY(return client_.joinOrCreate(to_std(room), (aurix::net::Mode)mode, max_players, to_std(props_json)), -1);
}
int AurixNetClient::join(const String &room, const String &schema_hash) {
    AURIX_TRY(return client_.join(to_std(room), to_std(schema_hash)), -1);
}
bool AurixNetClient::send_control(const String &json) { AURIX_TRY(client_.sendControl(to_std(json)); return true, false); }
bool AurixNetClient::send_event(int code, const PackedByteArray &payload, int target, bool reliable) {
    AURIX_TRY(client_.sendEvent((uint8_t)code, payload.ptr(), payload.size(), (aurix::net::Target)target, reliable); return true, false);
}
bool AurixNetClient::send_data(int channel, const PackedByteArray &payload) {
    AURIX_TRY(client_.sendData((aurix::net::Channel)channel, payload.ptr(), payload.size()); return true, false);
}
void AurixNetClient::flush() { if (client_.connected()) client_.flush(); }
int AurixNetClient::poll(int max_events) { return client_.connected() ? client_.poll(max_events) : 0; }
void AurixNetClient::leave() { client_.leave(); }
void AurixNetClient::disconnect_from() { client_.destroy(); }
void AurixNetClient::_process(double) { poll(); }
