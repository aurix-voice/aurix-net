#pragma once
#include <godot_cpp/classes/ref_counted.hpp>
#include <godot_cpp/variant/packed_byte_array.hpp>
#include <godot_cpp/variant/packed_int64_array.hpp>
#include <godot_cpp/variant/variant.hpp>
#include <memory>
#include "aurix_net.hpp"
#include "aurix_net_client.h"

namespace godot {

/// Client-side replicated world of a sync room: feed every `data` payload of an AurixNetClient to
/// receive(); it applies the delta, acks it and emits spawn/change/despawn. Fields are read by
/// component/field name as Godot values (Vector2/Vector3/Quaternion/float/String/PackedByteArray).
class AurixNetSync : public RefCounted {
    GDCLASS(AurixNetSync, RefCounted)
    std::unique_ptr<aurix::net::Sync> sync_;
    String last_error_;
    int current_ = -1;

protected:
    static void _bind_methods();

public:
    bool setup(const String &schema, int tick_hz = 60, int interpolation_ticks = 2);
    bool receive(AurixNetClient *client, const PackedByteArray &payload);
    int get_server_tick() const { return sync_ ? (int)sync_->serverTick() : 0; }
    int get_gaps() const { return sync_ ? (int)sync_->gaps() : 0; }
    double advance(double dt) { return sync_ ? sync_->advance(dt) : 0.0; }
    double get_render_tick() const { return sync_ ? sync_->renderTick() : 0.0; }
    PackedInt64Array get_entities() const;
    int get_owner(int64_t entity) const;
    bool has(int64_t entity, const String &component) const;
    Variant get_field(int64_t entity, const String &component, const String &field) const;
    int send_input(AurixNetClient *client, const PackedByteArray &body);
    bool begin_write(const String &component);
    bool write_field(const String &field, const Variant &value);
    bool end_write();
    int send_writes(AurixNetClient *client);
    String get_last_error() const { return last_error_; }
};

} // namespace godot
