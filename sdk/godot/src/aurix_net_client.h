#pragma once
#include "aurix_net.hpp"
#include <godot_cpp/classes/node.hpp>
#include <godot_cpp/variant/packed_byte_array.hpp>

namespace godot {

/// Node wrapper: polls the client in _process and emits signals.
class AurixNetClient : public Node {
	GDCLASS(AurixNetClient, Node)
	aurix::net::Client client_;
	bool relay_ = true; // mode of the joined room: sync and lockstep frames are not relay events
	void note_mode();
	bool fits(int64_t value, int64_t max, const char *what);
	String last_error_;
	int last_error_code_ = AURIX_NET_OK;
	bool hooked_ = false;
	void hook();

protected:
	static void _bind_methods();

public:
	/// The largest control message the node reads, in bytes of its JSON, and the largest sign-in.
	enum { MAX_CONTROL_MESSAGE = AURIX_NET_MAX_CONTROL_MESSAGE,
		MAX_CONTROL_MESSAGE_BEFORE_AUTH = AURIX_NET_MAX_CONTROL_MESSAGE_BEFORE_AUTH };
	enum Mode { MODE_RELAY = 0,
		MODE_SYNC = 1,
		MODE_LOCKSTEP = 2 };
	enum Channel { CHANNEL_UNRELIABLE = 0,
		CHANNEL_RELIABLE = 1,
		CHANNEL_RELIABLE_UNORDERED = 2,
		CHANNEL_UNRELIABLE_UNSEQUENCED = 4 };
	enum Target { TARGET_ALL = 0,
		TARGET_OTHERS = 1,
		TARGET_MASTER = 2 };
	enum ErrorCode { ERROR_NONE = AURIX_NET_OK,
		ERROR_INVALID_ARGUMENT = AURIX_NET_ERR_INVALID_ARGUMENT,
		ERROR_HTTP = AURIX_NET_ERR_HTTP,
		ERROR_CONNECTION = AURIX_NET_ERR_CONNECTION,
		ERROR_DATA_PATH = AURIX_NET_ERR_DATA_PATH,
		ERROR_DISCONNECTED = AURIX_NET_ERR_DISCONNECTED,
		ERROR_TIMEOUT = AURIX_NET_ERR_TIMEOUT,
		ERROR_PROTOCOL = AURIX_NET_ERR_PROTOCOL,
		ERROR_PANIC = AURIX_NET_ERR_PANIC,
		ERROR_UNAUTHORIZED = AURIX_NET_ERR_UNAUTHORIZED,
		ERROR_FORBIDDEN = AURIX_NET_ERR_FORBIDDEN,
		ERROR_NOT_FOUND = AURIX_NET_ERR_NOT_FOUND,
		ERROR_CONFLICT = AURIX_NET_ERR_CONFLICT,
		ERROR_INVALID_REQUEST = AURIX_NET_ERR_INVALID_REQUEST,
		ERROR_ROOM_FULL = AURIX_NET_ERR_ROOM_FULL,
		ERROR_ROOM_CLOSED = AURIX_NET_ERR_ROOM_CLOSED,
		ERROR_SCHEMA_MISMATCH = AURIX_NET_ERR_SCHEMA_MISMATCH,
		ERROR_QUOTA_EXCEEDED = AURIX_NET_ERR_QUOTA_EXCEEDED,
		ERROR_RATE_LIMITED = AURIX_NET_ERR_RATE_LIMITED,
		ERROR_LEFT = AURIX_NET_ERR_LEFT,
		ERROR_SERVER = AURIX_NET_ERR_SERVER };

	bool connect_to(const String &http_url, const String &grant, bool force_tunnel = false);
	int connect_and_join(const String &http_url, const String &grant, const String &join_json, bool force_tunnel = false);
	int join_or_create(const String &room, int mode = MODE_RELAY, int max_players = 8, const String &props_json = "", const PackedStringArray &lobby_props = PackedStringArray(), const PackedStringArray &reserved = PackedStringArray(), bool visible = true, bool open = true);
	int join_random(const String &filter_json = "", const String &expr = "", const String &match_type = "");
	int join(const String &room, const String &schema_hash = "");
	bool send_control(const String &json);
	/// GetRegions + ping: JSON {region, endpoint, rtt_ms, candidates}; empty on error.
	static String best_region(const String &catalog_http, int probes);
	bool set_view(const PackedInt64Array &viewpoints, const PackedInt64Array &interest);
	bool set_groups(const PackedInt32Array &join, const PackedInt32Array &leave);
	bool send_event(int code, const PackedByteArray &payload, int target = TARGET_OTHERS, bool reliable = false);
	bool send_event_on(int channel, int code, const PackedByteArray &payload, int target = TARGET_OTHERS);
	bool send_data(int channel, const PackedByteArray &payload);
	void flush();
	int poll(int max_events = 256);
	void leave();
	void disconnect_from();
	int get_actor() const { return client_.actor(); }
	bool is_connected_to() const { return client_.connected(); }
	String get_joined_json() const { return String::utf8(client_.joinedJson().c_str()); }
	String get_player_id() const { return String::utf8(client_.playerId().c_str()); }
	String get_player_name() const { return String::utf8(client_.playerName().c_str()); }
	int64_t get_seed() const { return static_cast<int64_t>(client_.seed().value_or(0)); }
	/// The avatar a sync room made for this client, -1 until the room announces it.
	int64_t get_avatar() const {
		auto avatar = client_.avatar();
		return avatar ? static_cast<int64_t>(*avatar) : -1;
	}
	String get_last_error() const { return last_error_; }
	int get_last_error_code() const { return last_error_code_; }
	aurix::net::Client &raw() { return client_; }

	void _process(double delta) override;
};

} // namespace godot
VARIANT_ENUM_CAST(godot::AurixNetClient::Mode);
VARIANT_ENUM_CAST(godot::AurixNetClient::Channel);
VARIANT_ENUM_CAST(godot::AurixNetClient::Target);
VARIANT_ENUM_CAST(godot::AurixNetClient::ErrorCode);
