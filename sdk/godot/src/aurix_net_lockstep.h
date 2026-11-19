#pragma once
#include "aurix_net.hpp"
#include "aurix_net_client.h"
#include <godot_cpp/classes/ref_counted.hpp>
#include <godot_cpp/variant/array.hpp>
#include <godot_cpp/variant/dictionary.hpp>
#include <godot_cpp/variant/packed_byte_array.hpp>
#include <memory>

namespace godot {

/// Client side of a lockstep room (mirrors the C++ `Lockstep` / Unity `LockstepClient`): feed every `data`
/// payload of an AurixNetClient to receive(); it emits `tick(tick, inputs)` for every confirmed tick (inputs =
/// Array of {actor, data}), `command(tick, actor, data)` for reliable commands (right before that tick),
/// `restore(tick, hash, state)` on late join / resync, `checkpoint_requested(tick)` (master) and
/// `dump_requested(tick)` (desync forensics). The game owns the deterministic simulation: step it in `tick`,
/// hash it (fnv1a or AurixSim hashes) and report_hash(); answer checkpoint/dump requests with the serialised state.
class AurixNetLockstep : public RefCounted {
	GDCLASS(AurixNetLockstep, RefCounted)
	std::unique_ptr<aurix::net::Lockstep> ls_;
	String last_error_;
	int last_error_code_ = AURIX_NET_OK;

protected:
	static void _bind_methods();

public:
	AurixNetLockstep();
	/// True when the payload was a lockstep frame (signals may have fired).
	bool receive(AurixNetClient *client, const PackedByteArray &payload);
	/// Sends this tick's local input; returns the tick it was stamped with, or -1.
	int64_t send_input(AurixNetClient *client, const PackedByteArray &data);
	/// Reliable command outside the input stream (1..AURIX_NET_MAX_COMMAND bytes); everyone gets it at the same tick via `command`.
	bool send_command(AurixNetClient *client, const PackedByteArray &data);
	bool report_hash(AurixNetClient *client, int64_t tick, int64_t hash);
	bool send_checkpoint(AurixNetClient *client, int64_t tick, int64_t hash, const PackedByteArray &state);
	bool send_dump(AurixNetClient *client, int64_t tick, int64_t hash, const PackedByteArray &state);
	static int64_t fnv1a(const PackedByteArray &data) { return (int64_t)aurix::net::Lockstep::fnv1a(data.ptr(), data.size()); }

	int64_t get_stat(int key) const { return ls_ ? ls_->get(key) : 0; }
	void set_option(int key, int64_t value) {
		if (ls_) {
			ls_->set(key, value);
		}
	}
	void set_input_delay(int delay, int min_delay = 1, int max_delay = 8, bool adaptive = true) {
		if (ls_) {
			ls_->setInputDelay(delay, min_delay, max_delay, adaptive);
		}
	}
	void set_input_redundancy(int count) {
		if (ls_) {
			ls_->setInputRedundancy(count);
		}
	}
	int get_input_delay() const { return ls_ ? (int)ls_->inputDelay() : 0; }
	int64_t get_confirmed_tick() const { return ls_ ? ls_->confirmedTick() : 0; }
	int64_t get_server_tick() const { return ls_ ? ls_->serverTick() : 0; }
	int get_last_margin() const { return ls_ ? ls_->lastMargin() : 0; }
	int64_t get_late_inputs() const { return ls_ ? ls_->lateInputs() : 0; }
	String get_last_error() const { return last_error_; }
	int get_last_error_code() const { return last_error_code_; }
};

} // namespace godot
