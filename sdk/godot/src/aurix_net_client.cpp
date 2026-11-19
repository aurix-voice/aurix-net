#include "aurix_net_client.h"
#include <godot_cpp/core/class_db.hpp>

namespace godot {

static std::string to_std(const String &text) {
	return std::string(text.utf8().get_data());
}
static PackedByteArray to_pba(const uint8_t *data, size_t len) {
	PackedByteArray bytes;
	bytes.resize(len);
	if (len) {
		memcpy(bytes.ptrw(), data, len);
	}
	return bytes;
}

void AurixNetClient::_bind_methods() {
	ClassDB::bind_method(D_METHOD("connect_to", "http_url", "grant", "force_tunnel"), &AurixNetClient::connect_to, DEFVAL(false));
	ClassDB::bind_method(D_METHOD("join_or_create", "room", "mode", "max_players", "props_json", "lobby_props"), &AurixNetClient::join_or_create, DEFVAL(MODE_RELAY), DEFVAL(8), DEFVAL(""), DEFVAL(PackedStringArray()));
	ClassDB::bind_method(D_METHOD("join", "room", "schema_hash"), &AurixNetClient::join, DEFVAL(""));
	ClassDB::bind_method(D_METHOD("send_control", "json"), &AurixNetClient::send_control);
	ClassDB::bind_static_method("AurixNetClient", D_METHOD("best_region", "catalog_http", "probes"), &AurixNetClient::best_region, DEFVAL(3));
	ClassDB::bind_method(D_METHOD("set_view", "viewpoints", "interest"), &AurixNetClient::set_view, DEFVAL(PackedInt64Array()));
	ClassDB::bind_method(D_METHOD("set_groups", "join", "leave"), &AurixNetClient::set_groups, DEFVAL(PackedInt32Array()));
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
	ClassDB::bind_method(D_METHOD("get_last_error_code"), &AurixNetClient::get_last_error_code);
	ADD_SIGNAL(MethodInfo("control", PropertyInfo(Variant::STRING, "json")));
	ADD_SIGNAL(MethodInfo("event", PropertyInfo(Variant::INT, "code"), PropertyInfo(Variant::INT, "from_actor"), PropertyInfo(Variant::PACKED_BYTE_ARRAY, "payload")));
	ADD_SIGNAL(MethodInfo("data", PropertyInfo(Variant::INT, "channel"), PropertyInfo(Variant::INT, "tick"), PropertyInfo(Variant::PACKED_BYTE_ARRAY, "payload")));
	ADD_SIGNAL(MethodInfo("closed", PropertyInfo(Variant::STRING, "reason")));
	ADD_SIGNAL(MethodInfo("notice", PropertyInfo(Variant::STRING, "text"), PropertyInfo(Variant::STRING, "level")));
	ADD_SIGNAL(MethodInfo("rehost"));
	BIND_ENUM_CONSTANT(MODE_RELAY);
	BIND_ENUM_CONSTANT(MODE_SYNC);
	BIND_ENUM_CONSTANT(MODE_LOCKSTEP);
	BIND_ENUM_CONSTANT(TARGET_ALL);
	BIND_ENUM_CONSTANT(TARGET_OTHERS);
	BIND_ENUM_CONSTANT(TARGET_MASTER);
	BIND_ENUM_CONSTANT(ERROR_NONE);
	BIND_ENUM_CONSTANT(ERROR_INVALID_ARGUMENT);
	BIND_ENUM_CONSTANT(ERROR_HTTP);
	BIND_ENUM_CONSTANT(ERROR_CONNECTION);
	BIND_ENUM_CONSTANT(ERROR_DATA_PATH);
	BIND_ENUM_CONSTANT(ERROR_DISCONNECTED);
	BIND_ENUM_CONSTANT(ERROR_TIMEOUT);
	BIND_ENUM_CONSTANT(ERROR_PROTOCOL);
	BIND_ENUM_CONSTANT(ERROR_UNAUTHORIZED);
	BIND_ENUM_CONSTANT(ERROR_FORBIDDEN);
	BIND_ENUM_CONSTANT(ERROR_NOT_FOUND);
	BIND_ENUM_CONSTANT(ERROR_CONFLICT);
	BIND_ENUM_CONSTANT(ERROR_INVALID_REQUEST);
	BIND_ENUM_CONSTANT(ERROR_ROOM_FULL);
	BIND_ENUM_CONSTANT(ERROR_ROOM_CLOSED);
	BIND_ENUM_CONSTANT(ERROR_SCHEMA_MISMATCH);
	BIND_ENUM_CONSTANT(ERROR_QUOTA_EXCEEDED);
	BIND_ENUM_CONSTANT(ERROR_RATE_LIMITED);
	BIND_ENUM_CONSTANT(ERROR_SERVER);
}

void AurixNetClient::hook() {
	if (hooked_) {
		return;
	}
	hooked_ = true;
	client_.onControl = [this](const std::string &json) { emit_signal("control", String::utf8(json.c_str())); };
	client_.onClosed = [this](const std::string &reason) { emit_signal("closed", String::utf8(reason.c_str())); };
	client_.onNotice = [this](const std::string &text, const std::string &level) { emit_signal("notice", String::utf8(text.c_str()), String::utf8(level.c_str())); };
	client_.onRehost = [this]() { emit_signal("rehost"); };
	client_.onData = [this](aurix::net::Channel ch, uint16_t tick, const uint8_t *data, size_t len) {
		emit_signal("data", (int)ch, (int)tick, to_pba(data, len));
		aurix::net::EventFrame frame;
		const bool service = len > 0 && ((data[0] & 0xf0) == 0x10 || (data[0] & 0xf0) == 0x20);
		if ((relay_ || !service) && aurix::net::EventFrame::decode(data, len, frame)) {
			emit_signal("event", (int)frame.code, (int)frame.from, to_pba(frame.payload, frame.len));
		}
	};
}

#define AURIX_TRY(expr, fail)                  \
	try {                                      \
		expr;                                  \
	} catch (const aurix::net::Error &error) { \
		last_error_ = error.what();            \
		last_error_code_ = error.code;         \
		return fail;                           \
	}

bool AurixNetClient::connect_to(const String &http_url, const String &grant, bool force_tunnel) {
	AURIX_TRY(client_.connect(to_std(http_url), to_std(grant), force_tunnel); hook(); return true, false);
}
void AurixNetClient::note_mode() {
	const std::string mode = aurix::net::Client::jsonStr(client_.joinedJson(), "mode");
	relay_ = mode != "sync" && mode != "lockstep";
}
int AurixNetClient::join_or_create(const String &room, int mode, int max_players, const String &props_json, const PackedStringArray &lobby_props) {
	std::vector<std::string> lobbyProps;
	for (int k = 0; k < lobby_props.size(); ++k) {
		lobbyProps.push_back(to_std(lobby_props[k]));
	}
	AURIX_TRY(const int actor = client_.joinOrCreate(to_std(room), (aurix::net::Mode)mode, max_players, to_std(props_json), lobbyProps); note_mode(); return actor, -1);
}
int AurixNetClient::join(const String &room, const String &schema_hash) {
	AURIX_TRY(const int actor = client_.join(to_std(room), to_std(schema_hash)); note_mode(); return actor, -1);
}
bool AurixNetClient::send_control(const String &json) {
	AURIX_TRY(client_.sendControl(to_std(json)); return true, false);
}
bool AurixNetClient::send_event(int code, const PackedByteArray &payload, int target, bool reliable) {
	AURIX_TRY(client_.sendEvent((uint8_t)code, payload.ptr(), payload.size(), (aurix::net::Target)target, reliable); return true, false);
}

bool AurixNetClient::set_view(const PackedInt64Array &viewpoints, const PackedInt64Array &interest) {
	std::vector<uint64_t> viewpointIds, interestIds;
	for (int k = 0; k < viewpoints.size(); ++k) {
		viewpointIds.push_back((uint64_t)viewpoints[k]);
	}
	for (int k = 0; k < interest.size(); ++k) {
		interestIds.push_back((uint64_t)interest[k]);
	}
	AURIX_TRY(client_.setView(viewpointIds, interestIds); return true, false)
}
bool AurixNetClient::set_groups(const PackedInt32Array &join, const PackedInt32Array &leave) {
	std::vector<uint16_t> joined, left;
	for (int k = 0; k < join.size(); ++k) {
		joined.push_back((uint16_t)join[k]);
	}
	for (int k = 0; k < leave.size(); ++k) {
		left.push_back((uint16_t)leave[k]);
	}
	AURIX_TRY(client_.setGroups(joined, left); return true, false)
}
bool AurixNetClient::send_data(int channel, const PackedByteArray &payload) {
	AURIX_TRY(client_.sendData((aurix::net::Channel)channel, payload.ptr(), payload.size()); return true, false);
}
void AurixNetClient::flush() {
	if (client_.connected()) {
		client_.flush();
	}
}
int AurixNetClient::poll(int max_events) {
	return client_.connected() ? client_.poll(max_events) : 0;
}
void AurixNetClient::leave() {
	client_.leave();
}
void AurixNetClient::disconnect_from() {
	client_.destroy();
}
void AurixNetClient::_process(double) {
	poll();
}

String AurixNetClient::best_region(const String &catalog_http, int probes) {
	try {
		return String::utf8(aurix::net::Client::bestRegionJson(to_std(catalog_http), probes).c_str());
	} catch (const std::exception &) {
		return String();
	}
}

} // namespace godot
