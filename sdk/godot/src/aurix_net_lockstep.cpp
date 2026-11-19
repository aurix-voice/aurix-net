#include "aurix_net_lockstep.h"
#include <cstring>
#include <godot_cpp/core/class_db.hpp>

namespace godot {

void AurixNetLockstep::_bind_methods() {
	ClassDB::bind_method(D_METHOD("receive", "client", "payload"), &AurixNetLockstep::receive);
	ClassDB::bind_method(D_METHOD("send_input", "client", "data"), &AurixNetLockstep::send_input);
	ClassDB::bind_method(D_METHOD("send_command", "client", "data"), &AurixNetLockstep::send_command);
	ClassDB::bind_method(D_METHOD("report_hash", "client", "tick", "hash"), &AurixNetLockstep::report_hash);
	ClassDB::bind_method(D_METHOD("send_checkpoint", "client", "tick", "hash", "state"), &AurixNetLockstep::send_checkpoint);
	ClassDB::bind_method(D_METHOD("send_dump", "client", "tick", "hash", "state"), &AurixNetLockstep::send_dump);
	ClassDB::bind_static_method("AurixNetLockstep", D_METHOD("fnv1a", "data"), &AurixNetLockstep::fnv1a);
	ClassDB::bind_method(D_METHOD("get_stat", "key"), &AurixNetLockstep::get_stat);
	ClassDB::bind_method(D_METHOD("set_option", "key", "value"), &AurixNetLockstep::set_option);
	ClassDB::bind_method(D_METHOD("set_input_delay", "delay", "min_delay", "max_delay", "adaptive"), &AurixNetLockstep::set_input_delay, DEFVAL(1), DEFVAL(8), DEFVAL(true));
	ClassDB::bind_method(D_METHOD("set_input_redundancy", "n"), &AurixNetLockstep::set_input_redundancy);
	ClassDB::bind_method(D_METHOD("get_input_delay"), &AurixNetLockstep::get_input_delay);
	ClassDB::bind_method(D_METHOD("get_confirmed_tick"), &AurixNetLockstep::get_confirmed_tick);
	ClassDB::bind_method(D_METHOD("get_server_tick"), &AurixNetLockstep::get_server_tick);
	ClassDB::bind_method(D_METHOD("get_last_margin"), &AurixNetLockstep::get_last_margin);
	ClassDB::bind_method(D_METHOD("get_late_inputs"), &AurixNetLockstep::get_late_inputs);
	ClassDB::bind_method(D_METHOD("get_last_error"), &AurixNetLockstep::get_last_error);
	ClassDB::bind_method(D_METHOD("get_last_error_code"), &AurixNetLockstep::get_last_error_code);
	ADD_SIGNAL(MethodInfo("tick", PropertyInfo(Variant::INT, "tick"), PropertyInfo(Variant::ARRAY, "inputs")));
	ADD_SIGNAL(MethodInfo("command", PropertyInfo(Variant::INT, "tick"), PropertyInfo(Variant::INT, "actor"), PropertyInfo(Variant::PACKED_BYTE_ARRAY, "data")));
	ADD_SIGNAL(MethodInfo("restore", PropertyInfo(Variant::INT, "tick"), PropertyInfo(Variant::INT, "hash"), PropertyInfo(Variant::PACKED_BYTE_ARRAY, "state")));
	ADD_SIGNAL(MethodInfo("checkpoint_requested", PropertyInfo(Variant::INT, "tick")));
	ADD_SIGNAL(MethodInfo("dump_requested", PropertyInfo(Variant::INT, "tick")));
}

AurixNetLockstep::AurixNetLockstep() :
		ls_(std::make_unique<aurix::net::Lockstep>()) {}

static PackedByteArray to_pba(const uint8_t *data, size_t len) {
	PackedByteArray out;
	out.resize((int64_t)len);
	if (len) {
		memcpy(out.ptrw(), data, len);
	}
	return out;
}

bool AurixNetLockstep::receive(AurixNetClient *client, const PackedByteArray &payload) {
	if (!ls_ || !client || !ls_->receive(client->raw(), payload.ptr(), payload.size())) {
		return false;
	}
	for (auto event = ls_->poll(); event.kind != aurix::net::Lockstep::Kind::None; event = ls_->poll()) {
		switch (event.kind) {
			case aurix::net::Lockstep::Kind::Tick: {
				Array inputs;
				for (const auto &in : event.inputs) {
					if (in.actor == AURIX_NET_COMMAND_ACTOR) {
						for (size_t offset = 0; offset + 4 <= in.len;) {
							const int actor = (in.data[offset] << 8) | in.data[offset + 1];
							const size_t len = (size_t)((in.data[offset + 2] << 8) | in.data[offset + 3]);
							offset += 4;
							if (offset + len > in.len) {
								break;
							}
							emit_signal("command", (int64_t)event.tick, actor, to_pba(in.data + offset, len));
							offset += len;
						}
						continue;
					}
					Dictionary input;
					input["actor"] = (int)in.actor;
					input["data"] = to_pba(in.data, in.len);
					inputs.push_back(input);
				}
				emit_signal("tick", (int64_t)event.tick, inputs);
				break;
			}
			case aurix::net::Lockstep::Kind::Restore:
				emit_signal("restore", (int64_t)event.tick, (int64_t)event.hash, to_pba(event.state, event.stateLen));
				break;
			case aurix::net::Lockstep::Kind::CheckpointReq:
				emit_signal("checkpoint_requested", (int64_t)event.tick);
				break;
			case aurix::net::Lockstep::Kind::DumpReq:
				emit_signal("dump_requested", (int64_t)event.tick);
				break;
			default:
				break;
		}
	}
	return true;
}

#define AURIX_TRY(fail, ...)                   \
	try {                                      \
		__VA_ARGS__;                           \
	} catch (const aurix::net::Error &error) { \
		last_error_ = error.what();            \
		last_error_code_ = error.code;         \
		return fail;                           \
	}

int64_t AurixNetLockstep::send_input(AurixNetClient *client, const PackedByteArray &data) {
	if (!ls_ || !client) {
		return -1;
	}
	return ls_->sendInput(client->raw(), data.ptr(), data.size());
}
bool AurixNetLockstep::send_command(AurixNetClient *client, const PackedByteArray &data) {
	if (!ls_ || !client) {
		return false;
	}
	std::vector<uint8_t> bytes(data.ptr(), data.ptr() + data.size());
	AURIX_TRY(false, ls_->sendCommand(client->raw(), bytes); return true);
}
bool AurixNetLockstep::report_hash(AurixNetClient *client, int64_t tick, int64_t hash) {
	if (!client) {
		return false;
	}
	aurix::net::Lockstep::reportHash(client->raw(), (uint32_t)tick, (uint64_t)hash);
	return true;
}
bool AurixNetLockstep::send_checkpoint(AurixNetClient *client, int64_t tick, int64_t hash, const PackedByteArray &state) {
	if (!client) {
		return false;
	}
	aurix::net::Lockstep::sendCheckpoint(client->raw(), (uint32_t)tick, (uint64_t)hash, state.ptr(), state.size());
	return true;
}
bool AurixNetLockstep::send_dump(AurixNetClient *client, int64_t tick, int64_t hash, const PackedByteArray &state) {
	if (!client) {
		return false;
	}
	aurix::net::Lockstep::sendDump(client->raw(), (uint32_t)tick, (uint64_t)hash, state.ptr(), state.size());
	return true;
}

} // namespace godot
