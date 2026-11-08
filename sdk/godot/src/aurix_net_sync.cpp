#include "aurix_net_sync.h"
#include <godot_cpp/core/class_db.hpp>

namespace godot {

static std::string to_std(const String &s) { return std::string(s.utf8().get_data()); }

void AurixNetSync::_bind_methods() {
    ClassDB::bind_method(D_METHOD("setup", "schema", "tick_hz", "interpolation_ticks"), &AurixNetSync::setup, DEFVAL(60), DEFVAL(2));
    ClassDB::bind_method(D_METHOD("receive", "client", "payload"), &AurixNetSync::receive);
    ClassDB::bind_method(D_METHOD("get_server_tick"), &AurixNetSync::get_server_tick);
    ClassDB::bind_method(D_METHOD("get_gaps"), &AurixNetSync::get_gaps);
    ClassDB::bind_method(D_METHOD("advance", "dt"), &AurixNetSync::advance);
    ClassDB::bind_method(D_METHOD("get_render_tick"), &AurixNetSync::get_render_tick);
    ClassDB::bind_method(D_METHOD("get_entities"), &AurixNetSync::get_entities);
    ClassDB::bind_method(D_METHOD("get_owner", "entity"), &AurixNetSync::get_owner);
    ClassDB::bind_method(D_METHOD("has", "entity", "component"), &AurixNetSync::has);
    ClassDB::bind_method(D_METHOD("get_field", "entity", "component", "field"), &AurixNetSync::get_field);
    ClassDB::bind_method(D_METHOD("send_input", "client", "body"), &AurixNetSync::send_input);
    ClassDB::bind_method(D_METHOD("begin_write", "component"), &AurixNetSync::begin_write);
    ClassDB::bind_method(D_METHOD("write_field", "field", "value"), &AurixNetSync::write_field);
    ClassDB::bind_method(D_METHOD("end_write"), &AurixNetSync::end_write);
    ClassDB::bind_method(D_METHOD("send_writes", "client"), &AurixNetSync::send_writes);
    ClassDB::bind_method(D_METHOD("write_entity", "client", "entity"), &AurixNetSync::write_entity);
    ClassDB::bind_method(D_METHOD("spawn_entity", "client"), &AurixNetSync::spawn_entity);
    ClassDB::bind_method(D_METHOD("despawn_entity", "client", "entity"), &AurixNetSync::despawn_entity);
    ClassDB::bind_method(D_METHOD("transfer_entity", "client", "entity", "to_actor"), &AurixNetSync::transfer_entity);
    ClassDB::bind_method(D_METHOD("get_last_error"), &AurixNetSync::get_last_error);
    ADD_SIGNAL(MethodInfo("spawn", PropertyInfo(Variant::INT, "entity"), PropertyInfo(Variant::INT, "owner")));
    ADD_SIGNAL(MethodInfo("change", PropertyInfo(Variant::INT, "entity"), PropertyInfo(Variant::INT, "owner")));
    ADD_SIGNAL(MethodInfo("despawn", PropertyInfo(Variant::INT, "entity")));
}

#define AURIX_TRY(fail, ...) try { __VA_ARGS__; } catch (const std::exception &e) { last_error_ = e.what(); return fail; }

bool AurixNetSync::setup(const String &schema, int tick_hz, int interpolation_ticks) {
    AURIX_TRY(false, sync_ = std::make_unique<aurix::net::Sync>(to_std(schema), tick_hz, interpolation_ticks); return true);
}

bool AurixNetSync::receive(AurixNetClient *client, const PackedByteArray &payload) {
    if (!sync_ || !client) return false;
    bool delta = false;
    AURIX_TRY(false, delta = sync_->receive(client->raw(), payload.ptr(), payload.size()));
    if (!delta) return false;
    for (auto e = sync_->poll(); e.kind != aurix::net::Sync::Kind::None; e = sync_->poll()) {
        switch (e.kind) {
            case aurix::net::Sync::Kind::Spawn: emit_signal("spawn", (int64_t)e.entity, e.owner); break;
            case aurix::net::Sync::Kind::Change: emit_signal("change", (int64_t)e.entity, e.owner); break;
            case aurix::net::Sync::Kind::Despawn: emit_signal("despawn", (int64_t)e.entity); break;
            default: break;
        }
    }
    return true;
}

PackedInt64Array AurixNetSync::get_entities() const {
    PackedInt64Array out;
    if (!sync_) return out;
    uint64_t id; int owner;
    for (int i = 0; sync_->entityAt(i, id, owner); i++) out.push_back((int64_t)id);
    return out;
}

int AurixNetSync::get_owner(int64_t entity) const {
    if (!sync_) return -1;
    uint64_t id; int owner;
    for (int i = 0; sync_->entityAt(i, id, owner); i++) if (id == (uint64_t)entity) return owner;
    return -1;
}

bool AurixNetSync::has(int64_t entity, const String &component) const {
    if (!sync_) return false;
    int c = sync_->component(to_std(component).c_str());
    return c >= 0 && sync_->has((uint64_t)entity, c);
}

Variant AurixNetSync::get_field(int64_t entity, const String &component, const String &field) const {
    if (!sync_) return Variant();
    int c = sync_->component(to_std(component).c_str());
    int f = c >= 0 ? sync_->field(c, to_std(field).c_str()) : -1;
    if (f < 0) return Variant();
    float v[4] = {0, 0, 0, 0};
    switch (sync_->vector((uint64_t)entity, c, f, v)) {
        case 2: return Vector2(v[0], v[1]);
        case 3: return Vector3(v[0], v[1], v[2]);
        case 4: return Quaternion(v[0], v[1], v[2], v[3]);
        case 1: { double d; if (sync_->number((uint64_t)entity, c, f, d)) return d; return (double)v[0]; }
        default: break;
    }
    std::string t = sync_->text((uint64_t)entity, c, f);
    return t.empty() && !sync_->has((uint64_t)entity, c) ? Variant() : Variant(String::utf8(t.c_str(), (int)t.size()));
}

int AurixNetSync::send_input(AurixNetClient *client, const PackedByteArray &body) {
    if (!sync_ || !client) return -1;
    return (int)sync_->sendInput(client->raw(), body.ptr(), body.size());
}

bool AurixNetSync::begin_write(const String &component) {
    if (!sync_) return false;
    int c = sync_->component(to_std(component).c_str());
    if (c < 0) { last_error_ = "unknown component " + component; return false; }
    AURIX_TRY(false, sync_->begin(c); current_ = c; return true);
}

bool AurixNetSync::write_field(const String &field, const Variant &value) {
    if (!sync_) return false;
    // the component under construction is the last begin(); resolve the field through it
    AURIX_TRY(false, {
        int c = current_;
        int f = sync_->field(c, to_std(field).c_str());
        if (f < 0) { last_error_ = "unknown field " + field; return false; }
        switch (value.get_type()) {
            case Variant::VECTOR2: { Vector2 a = value; float v[2] = {(float)a.x, (float)a.y}; sync_->write(f, v, 2); break; }
            case Variant::VECTOR3: { Vector3 a = value; float v[3] = {(float)a.x, (float)a.y, (float)a.z}; sync_->write(f, v, 3); break; }
            case Variant::QUATERNION: { Quaternion q = value; float v[4] = {(float)q.x, (float)q.y, (float)q.z, (float)q.w}; sync_->write(f, v, 4); break; }
            case Variant::STRING: { std::string s = to_std(value); sync_->write(f, s); break; }
            case Variant::PACKED_BYTE_ARRAY: { PackedByteArray b = value; sync_->write(f, b.ptr(), b.size()); break; }
            default: sync_->write(f, (double)value); break;
        }
        return true;
    });
    return false;
}

bool AurixNetSync::end_write() {
    if (!sync_) return false;
    AURIX_TRY(false, sync_->end(); return true);
}

int AurixNetSync::send_writes(AurixNetClient *client) {
    if (!sync_ || !client) return -1;
    return (int)sync_->sendWrites(client->raw());
}

// Owner-authoritative ops on entities this actor owns (queued begin_write/end_write feed write/spawn).
int AurixNetSync::write_entity(AurixNetClient *client, int64_t entity) {
    if (!sync_ || !client) return -1;
    return (int)sync_->writeEntity(client->raw(), (uint64_t)entity);
}
int AurixNetSync::spawn_entity(AurixNetClient *client) {
    if (!sync_ || !client) return -1;
    return (int)sync_->spawn(client->raw());
}
int AurixNetSync::despawn_entity(AurixNetClient *client, int64_t entity) {
    if (!sync_ || !client) return -1;
    return (int)sync_->despawn(client->raw(), (uint64_t)entity);
}
int AurixNetSync::transfer_entity(AurixNetClient *client, int64_t entity, int to_actor) {
    if (!sync_ || !client) return -1;
    return (int)sync_->transfer(client->raw(), (uint64_t)entity, to_actor);
}

} // namespace godot
