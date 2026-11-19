#include "aurix_net_sync.h"
#include <godot_cpp/core/class_db.hpp>

namespace godot {

static std::string to_std(const String &text) {
	return std::string(text.utf8().get_data());
}

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
	ClassDB::bind_method(D_METHOD("set_policy", "client", "entity", "policy"), &AurixNetSync::set_policy);
	ClassDB::bind_method(D_METHOD("request_ownership", "client", "entity"), &AurixNetSync::request_ownership);
	ClassDB::bind_method(D_METHOD("release_entity", "client", "entity"), &AurixNetSync::release_entity);
	ClassDB::bind_method(D_METHOD("get_last_error"), &AurixNetSync::get_last_error);
	ClassDB::bind_method(D_METHOD("get_last_error_code"), &AurixNetSync::get_last_error_code);
	ADD_SIGNAL(MethodInfo("spawn", PropertyInfo(Variant::INT, "entity"), PropertyInfo(Variant::INT, "owner")));
	ADD_SIGNAL(MethodInfo("change", PropertyInfo(Variant::INT, "entity"), PropertyInfo(Variant::INT, "owner")));
	ADD_SIGNAL(MethodInfo("despawn", PropertyInfo(Variant::INT, "entity")));
}

#define AURIX_TRY(fail, ...)                   \
	try {                                      \
		__VA_ARGS__;                           \
	} catch (const aurix::net::Error &error) { \
		last_error_ = error.what();            \
		last_error_code_ = error.code;         \
		return fail;                           \
	}

bool AurixNetSync::setup(const String &schema, int tick_hz, int interpolation_ticks) {
	AURIX_TRY(false, sync_ = std::make_unique<aurix::net::Sync>(to_std(schema), tick_hz, interpolation_ticks); return true);
}

bool AurixNetSync::receive(AurixNetClient *client, const PackedByteArray &payload) {
	if (!sync_ || !client) {
		return false;
	}
	bool delta = false;
	AURIX_TRY(false, delta = sync_->receive(client->raw(), payload.ptr(), payload.size()));
	if (!delta) {
		return false;
	}
	for (auto event = sync_->poll(); event.kind != aurix::net::Sync::Kind::None; event = sync_->poll()) {
		switch (event.kind) {
			case aurix::net::Sync::Kind::Spawn:
				emit_signal("spawn", (int64_t)event.entity, event.owner);
				break;
			case aurix::net::Sync::Kind::Change:
				emit_signal("change", (int64_t)event.entity, event.owner);
				break;
			case aurix::net::Sync::Kind::Despawn:
				emit_signal("despawn", (int64_t)event.entity);
				break;
			default:
				break;
		}
	}
	return true;
}

PackedInt64Array AurixNetSync::get_entities() const {
	PackedInt64Array out;
	if (!sync_) {
		return out;
	}
	uint64_t id;
	int owner;
	for (int i = 0; sync_->entityAt(i, id, owner); i++) {
		out.push_back((int64_t)id);
	}
	return out;
}

int AurixNetSync::get_owner(int64_t entity) const {
	if (!sync_) {
		return -1;
	}
	uint64_t id;
	int owner;
	for (int i = 0; sync_->entityAt(i, id, owner); i++) {
		if (id == (uint64_t)entity) {
			return owner;
		}
	}
	return -1;
}

bool AurixNetSync::has(int64_t entity, const String &component) const {
	if (!sync_) {
		return false;
	}
	int componentId = sync_->component(to_std(component).c_str());
	return componentId >= 0 && sync_->has((uint64_t)entity, componentId);
}

Variant AurixNetSync::get_field(int64_t entity, const String &component, const String &field) const {
	if (!sync_) {
		return Variant();
	}
	int componentId = sync_->component(to_std(component).c_str());
	int fieldId = componentId >= 0 ? sync_->field(componentId, to_std(field).c_str()) : -1;
	if (fieldId < 0) {
		return Variant();
	}
	float values[4] = { 0, 0, 0, 0 };
	switch (sync_->vector((uint64_t)entity, componentId, fieldId, values)) {
		case 2:
			return Vector2(values[0], values[1]);
		case 3:
			return Vector3(values[0], values[1], values[2]);
		case 4:
			return Quaternion(values[0], values[1], values[2], values[3]);
		case 1: {
			double number;
			if (sync_->number((uint64_t)entity, componentId, fieldId, number)) {
				return number;
			}
			return (double)values[0];
		}
		default:
			break;
	}
	std::string text = sync_->text((uint64_t)entity, componentId, fieldId);
	return text.empty() && !sync_->has((uint64_t)entity, componentId) ? Variant() : Variant(String::utf8(text.c_str(), (int)text.size()));
}

int AurixNetSync::send_input(AurixNetClient *client, const PackedByteArray &body) {
	if (!sync_ || !client) {
		return -1;
	}
	return (int)sync_->sendInput(client->raw(), body.ptr(), body.size());
}

bool AurixNetSync::begin_write(const String &component) {
	if (!sync_) {
		return false;
	}
	int componentId = sync_->component(to_std(component).c_str());
	if (componentId < 0) {
		last_error_ = "unknown component " + component;
		return false;
	}
	AURIX_TRY(false, sync_->begin(componentId); current_ = componentId; return true);
}

bool AurixNetSync::write_field(const String &field, const Variant &value) {
	if (!sync_) {
		return false;
	}
	// the component under construction is the last begin(); resolve the field through it
	AURIX_TRY(false, {
		int componentId = current_;
		int fieldId = sync_->field(componentId, to_std(field).c_str());
		if (fieldId < 0) {
			last_error_ = "unknown field " + field;
			return false;
		}
		switch (value.get_type()) {
			case Variant::VECTOR2: {
				Vector2 vector = value;
				float values[2] = { (float)vector.x, (float)vector.y };
				sync_->write(fieldId, values, 2);
				break;
			}
			case Variant::VECTOR3: {
				Vector3 vector = value;
				float values[3] = { (float)vector.x, (float)vector.y, (float)vector.z };
				sync_->write(fieldId, values, 3);
				break;
			}
			case Variant::QUATERNION: {
				Quaternion rotation = value;
				float values[4] = { (float)rotation.x, (float)rotation.y, (float)rotation.z, (float)rotation.w };
				sync_->write(fieldId, values, 4);
				break;
			}
			case Variant::STRING: {
				std::string text = to_std(value);
				sync_->write(fieldId, text);
				break;
			}
			case Variant::PACKED_BYTE_ARRAY: {
				PackedByteArray bytes = value;
				sync_->write(fieldId, bytes.ptr(), bytes.size());
				break;
			}
			default:
				sync_->write(fieldId, (double)value);
				break;
		}
		return true;
	});
	return false;
}

bool AurixNetSync::end_write() {
	if (!sync_) {
		return false;
	}
	AURIX_TRY(false, sync_->end(); return true);
}

int AurixNetSync::send_writes(AurixNetClient *client) {
	if (!sync_ || !client) {
		return -1;
	}
	return (int)sync_->sendWrites(client->raw());
}

// Owner-authoritative ops on entities this actor owns (queued begin_write/end_write feed write/spawn).
int AurixNetSync::write_entity(AurixNetClient *client, int64_t entity) {
	if (!sync_ || !client) {
		return -1;
	}
	return (int)sync_->writeEntity(client->raw(), (uint64_t)entity);
}
int AurixNetSync::spawn_entity(AurixNetClient *client) {
	if (!sync_ || !client) {
		return -1;
	}
	return (int)sync_->spawn(client->raw());
}
int AurixNetSync::despawn_entity(AurixNetClient *client, int64_t entity) {
	if (!sync_ || !client) {
		return -1;
	}
	return (int)sync_->despawn(client->raw(), (uint64_t)entity);
}
int AurixNetSync::transfer_entity(AurixNetClient *client, int64_t entity, int to_actor) {
	if (!sync_ || !client) {
		return -1;
	}
	return (int)sync_->transfer(client->raw(), (uint64_t)entity, to_actor);
}

int AurixNetSync::set_policy(AurixNetClient *client, int64_t entity, int policy) {
	if (!sync_ || !client) {
		return -1;
	}
	return (int)sync_->setPolicy(client->raw(), (uint64_t)entity, policy);
}
int AurixNetSync::request_ownership(AurixNetClient *client, int64_t entity) {
	if (!sync_ || !client) {
		return -1;
	}
	return (int)sync_->requestOwnership(client->raw(), (uint64_t)entity);
}
int AurixNetSync::release_entity(AurixNetClient *client, int64_t entity) {
	if (!sync_ || !client) {
		return -1;
	}
	return (int)sync_->release(client->raw(), (uint64_t)entity);
}

} // namespace godot
