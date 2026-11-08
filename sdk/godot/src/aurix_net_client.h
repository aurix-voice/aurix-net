#pragma once
#include <godot_cpp/classes/node.hpp>
#include <godot_cpp/variant/packed_byte_array.hpp>
#include "aurix_net.hpp"

namespace godot {

/// Node wrapper: polls the client in _process and emits signals.
class AurixNetClient : public Node {
    GDCLASS(AurixNetClient, Node)
    aurix::net::Client client_;
    String last_error_;
    bool hooked_ = false;
    void hook();

protected:
    static void _bind_methods();

public:
    enum Mode { MODE_RELAY = 0, MODE_SYNC = 1, MODE_LOCKSTEP = 2 };
    enum Target { TARGET_ALL = 0, TARGET_OTHERS = 1, TARGET_MASTER = 2 };

    bool connect_to(const String &http_url, const String &grant, bool force_tunnel = false);
    int join_or_create(const String &room, int mode = MODE_RELAY, int max_players = 8, const String &props_json = "");
    int join(const String &room, const String &schema_hash = "");
    bool send_control(const String &json);
    bool send_event(int code, const PackedByteArray &payload, int target = TARGET_OTHERS, bool reliable = false);
    bool send_data(int channel, const PackedByteArray &payload);
    void flush();
    int poll(int max_events = 256);
    void leave();
    void disconnect_from();
    int get_actor() const { return client_.actor(); }
    bool is_connected_to() const { return client_.connected(); }
    String get_joined_json() const { return String::utf8(client_.joinedJson().c_str()); }
    String get_last_error() const { return last_error_; }
    aurix::net::Client &raw() { return client_; }

    void _process(double delta) override;
};

} // namespace godot
VARIANT_ENUM_CAST(godot::AurixNetClient::Mode);
VARIANT_ENUM_CAST(godot::AurixNetClient::Target);
