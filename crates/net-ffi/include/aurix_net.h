/* Aurix Net C ABI. All calls are thread-safe; events are polled. Strings are UTF-8, NUL-terminated. */
#ifndef AURIX_NET_H
#define AURIX_NET_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

typedef struct AurixNetClient AurixNetClient;

enum {
    AURIX_NET_EVENT_NONE = 0,
    AURIX_NET_EVENT_CONTROL = 1,    /* json = server control message */
    AURIX_NET_EVENT_DATA = 2,       /* data/data_len on channel */
    AURIX_NET_EVENT_DATA_READY = 3,
    AURIX_NET_EVENT_CLOSED = 4      /* json = reason */
};

/* AURIX_NET_CH_UNRELIABLE drops a message that arrives after a newer one; AURIX_NET_CH_UNRELIABLE_UNSEQUENCED
 * delivers every message that arrives, in the order of arrival. */
enum {
    AURIX_NET_CH_UNRELIABLE = 0,
    AURIX_NET_CH_RELIABLE = 1,
    AURIX_NET_CH_RELIABLE_UNORDERED = 2,
    AURIX_NET_CH_UNRELIABLE_UNSEQUENCED = 4
};
enum { AURIX_NET_TARGET_ALL = 0, AURIX_NET_TARGET_OTHERS = 1, AURIX_NET_TARGET_MASTER = 2, AURIX_NET_TARGET_ACTORS = 3 };

typedef struct AurixNetEvent {
    int32_t kind;
    uint8_t channel;
    uint16_t tick;
    uint8_t *data;
    size_t data_len;
    char *json;
} AurixNetEvent;

/* Why the last failed call on this thread failed. aurix_net_last_error() has the text. Both are set by a call that
 * reports failure (NULL, -1); a call that succeeds leaves them as they were. */
enum {
  AURIX_NET_OK = 0,
  AURIX_NET_ERR_INVALID_ARGUMENT = 1, /* NULL, non-UTF-8 or out-of-range argument, or a call out of order */
  AURIX_NET_ERR_HTTP = 2,             /* an HTTP request to the node failed */
  AURIX_NET_ERR_CONNECTION = 3,       /* the control connection could not be opened, or broke */
  AURIX_NET_ERR_DATA_PATH = 4,        /* the data path could not be opened */
  AURIX_NET_ERR_DISCONNECTED = 5,     /* the connection is gone, or the client is not in a room */
  AURIX_NET_ERR_TIMEOUT = 6,
  AURIX_NET_ERR_PROTOCOL = 7,         /* a frame from the node could not be decoded */
  /* the node refused the request */
  AURIX_NET_ERR_UNAUTHORIZED = 100,
  AURIX_NET_ERR_FORBIDDEN = 101,
  AURIX_NET_ERR_NOT_FOUND = 102,      /* join: no such room */
  AURIX_NET_ERR_CONFLICT = 103,
  AURIX_NET_ERR_INVALID_REQUEST = 104,
  AURIX_NET_ERR_ROOM_FULL = 105,
  AURIX_NET_ERR_ROOM_CLOSED = 106,
  AURIX_NET_ERR_SCHEMA_MISMATCH = 107,
  AURIX_NET_ERR_QUOTA_EXCEEDED = 108,
  AURIX_NET_ERR_RATE_LIMITED = 109,
  AURIX_NET_ERR_SERVER = 199          /* any other failure on the node */
};

const char *aurix_net_version(void);
const char *aurix_net_last_error(void);
int32_t aurix_net_last_error_code(void);

/* backend/tooling only */
char *aurix_net_issue_grant(const char *http, const char *api_key, const char *player_id, const char *name);
char *aurix_net_best_region(const char *catalog_http, int32_t probes); /* GetRegions + ping: JSON {region, endpoint, rtt_ms, candidates}; NULL on error; free with aurix_net_string_free */
char *aurix_net_json_string(const char *json, const char *key); /* a top-level string field of a JSON object, e.g. "t" of a control message; NULL when absent or not a string; free with aurix_net_string_free */
void aurix_net_string_free(char *string);

AurixNetClient *aurix_net_connect(const char *http, const char *grant, int32_t tunnel);
int32_t aurix_net_send_control(AurixNetClient *client, const char *json);
int32_t aurix_net_join(AurixNetClient *client, const char *json);          /* returns actor id or -1 */
/* connect + join in one call; when the room lives on another node of the region the returned client is
 * connected to that node. *actor (may be NULL) gets the actor id. NULL on failure. */
AurixNetClient *aurix_net_connect_and_join(const char *http, const char *grant, int32_t tunnel, const char *json, int32_t *actor);
char *aurix_net_player_id(AurixNetClient *client);   /* the player id of the grant the client signed in with; free with aurix_net_string_free */
char *aurix_net_player_name(AurixNetClient *client); /* its display name; free with aurix_net_string_free */
char *aurix_net_joined_json(AurixNetClient *client);
int32_t aurix_net_joined_seed(AurixNetClient *client, uint64_t *seed);     /* seed of the lockstep room; 0, or -1 when not in one */
int32_t aurix_net_avatar(AurixNetClient *client, uint64_t *entity); /* sync rooms: 1 = the avatar the room made for this client, kept from the join on; 0 = not announced yet */
int32_t aurix_net_send_data(AurixNetClient *client, uint8_t channel, const uint8_t *data, size_t len);
int32_t aurix_net_send_event(AurixNetClient *client, uint8_t code, int32_t target, const uint16_t *actors, size_t n_actors,
                             const uint8_t *data, size_t len, int32_t reliable);
/* send_event + interest group (-1 = none) and cache (0 none, 1 add for late joiners, 2 remove) */
int32_t aurix_net_send_event_ex(AurixNetClient *client, uint8_t code, int32_t target, const uint16_t *actors, size_t n_actors,
                                const uint8_t *data, size_t len, int32_t reliable, int32_t group, int32_t cache);
/* send_event_ex on a channel of the caller's choice; a node that does not take AURIX_NET_CH_UNRELIABLE_UNSEQUENCED
 * gets its events on AURIX_NET_CH_UNRELIABLE */
int32_t aurix_net_send_event_on(AurixNetClient *client, uint8_t channel, uint8_t code, int32_t target, const uint16_t *actors,
                                size_t n_actors, const uint8_t *data, size_t len, int32_t group, int32_t cache);
void aurix_net_flush(AurixNetClient *client);
int32_t aurix_net_poll(AurixNetClient *client, AurixNetEvent *out);          /* 1 = event filled */
void aurix_net_event_free(AurixNetEvent *ev);
int32_t aurix_net_leave(AurixNetClient *client);
void aurix_net_destroy(AurixNetClient *client);

/* ---- lockstep rooms (pure-logic helper; feed it every data payload of a lockstep room) ---- */
typedef struct AurixNetLockstep AurixNetLockstep;
enum { AURIX_NET_LS_NONE = 0, AURIX_NET_LS_TICK = 1, AURIX_NET_LS_RESTORE = 2, AURIX_NET_LS_CHECKPOINT_REQ = 3, AURIX_NET_LS_DUMP_REQ = 4 };
enum { AURIX_NET_LS_INPUT_DELAY = 0, AURIX_NET_LS_INPUT_DELAY_MIN = 1, AURIX_NET_LS_INPUT_DELAY_MAX = 2, AURIX_NET_LS_ADAPTIVE_DELAY = 3,
       AURIX_NET_LS_INPUT_REDUNDANCY = 4, AURIX_NET_LS_LAST_MARGIN = 5, AURIX_NET_LS_LATE_INPUTS = 6, AURIX_NET_LS_CONFIRMED_TICK = 7,
       AURIX_NET_LS_SERVER_TICK = 8, AURIX_NET_LS_DELIVERED = 9, AURIX_NET_LS_CATCHUPS = 10, AURIX_NET_LS_DROPPED = 11 };
typedef struct AurixNetLsEvent {
  int32_t kind; uint32_t tick; uint64_t hash;
  uint16_t inputs;             /* TICK: actors with input, read with aurix_net_lockstep_input */
  const uint8_t *state; size_t state_len; /* RESTORE: checkpoint state, valid until the next poll */
} AurixNetLsEvent;
AurixNetLockstep *aurix_net_lockstep_new(void);
void aurix_net_lockstep_destroy(AurixNetLockstep *ls);
/* 1 = lockstep frame. Bundles are acknowledged through client so the room resends lost ones; NULL sends nothing. */
int32_t aurix_net_lockstep_receive(AurixNetLockstep *ls, AurixNetClient *client, const uint8_t *data, size_t len);
int32_t aurix_net_lockstep_poll(AurixNetLockstep *ls, AurixNetLsEvent *out);               /* 1 = event filled */
int32_t aurix_net_lockstep_input(AurixNetLockstep *ls, uint16_t index, uint16_t *actor, const uint8_t **data, size_t *len);
int64_t aurix_net_lockstep_send_input(AurixNetLockstep *ls, AurixNetClient *client, const uint8_t *data, size_t len); /* stamped tick; -1 on error or when this tick's input is already sent (one per room tick) */
int32_t aurix_net_lockstep_report_hash(AurixNetClient *client, uint32_t tick, uint64_t hash);
/* Reliable command outside the input stream: arrives in the next tick's inputs for every client under
   actor AURIX_NET_COMMAND_ACTOR as [u16 actor, u16 len, bytes]*; journaled, replayed, seen by the referee.
   1..AURIX_NET_MAX_COMMAND bytes, the longest command a tick can carry. */
#define AURIX_NET_COMMAND_ACTOR 0xFFFF
#define AURIX_NET_MAX_COMMAND 251
int32_t aurix_net_lockstep_send_command(AurixNetClient *client, const uint8_t *data, size_t len);
int32_t aurix_net_lockstep_send_checkpoint(AurixNetClient *client, uint32_t tick, uint64_t hash, const uint8_t *state, size_t len);
int32_t aurix_net_lockstep_send_dump(AurixNetClient *client, uint32_t tick, uint64_t hash, const uint8_t *state, size_t len);
int64_t aurix_net_lockstep_get(AurixNetLockstep *ls, int32_t key);
int32_t aurix_net_lockstep_set(AurixNetLockstep *ls, int32_t key, int64_t value);
uint64_t aurix_net_lockstep_fnv1a(const uint8_t *data, size_t len);

/* ---- sync rooms (client-side replicated world; feed it every data payload of a sync room) ---- */
typedef struct AurixNetSync AurixNetSync;
enum { AURIX_NET_SYNC_NONE = 0, AURIX_NET_SYNC_SPAWN = 1, AURIX_NET_SYNC_CHANGE = 2, AURIX_NET_SYNC_DESPAWN = 3 };
typedef struct AurixNetSyncEvent { int32_t kind; uint64_t entity; int32_t owner; /* -1 = server-owned */ } AurixNetSyncEvent;
enum { AURIX_NET_SPAWN_CREATED = 0, AURIX_NET_SPAWN_REFUSED = 1, AURIX_NET_SPAWN_INVALID = 2 };
typedef struct AurixNetSyncSpawn { uint32_t tag; /* 0 = the avatar */ int32_t status; uint64_t entity; /* read only when created: 0 is an entity too */ uint32_t tick; } AurixNetSyncSpawn;
AurixNetSync *aurix_net_sync_new(const char *schema);                                   /* NULL + last_error on a bad schema */
void aurix_net_sync_destroy(AurixNetSync *sync);
int32_t aurix_net_sync_receive(const AurixNetSync *sync, const uint8_t *data, size_t len, uint8_t *ack9); /* 1 = delta applied, ack9 filled (send it unreliable); 2 = input margin report taken; 3 = spawn answer taken; 0 = not a sync frame; -1 = error */
int32_t aurix_net_sync_poll(const AurixNetSync *sync, AurixNetSyncEvent *out);           /* 1 = spawn/change/despawn filled */
int32_t aurix_net_sync_poll_spawn(const AurixNetSync *sync, AurixNetSyncSpawn *out);     /* 1 = answer to aurix_net_sync_spawn filled; it and the delta with the entity come in either order */
uint32_t aurix_net_sync_server_tick(const AurixNetSync *sync);
uint64_t aurix_net_sync_gaps(const AurixNetSync *sync);
int32_t aurix_net_sync_entity_count(const AurixNetSync *sync);
int32_t aurix_net_sync_entity_at(const AurixNetSync *sync, int32_t index, uint64_t *id, int32_t *owner); /* 1 = filled */
int32_t aurix_net_sync_component(const AurixNetSync *sync, const char *name);              /* component id or -1 */
int32_t aurix_net_sync_field(const AurixNetSync *sync, int32_t component, const char *name); /* field index or -1 */
int32_t aurix_net_sync_has(const AurixNetSync *sync, uint64_t entity, int32_t component);
int32_t aurix_net_sync_get_number(const AurixNetSync *sync, uint64_t entity, int32_t component, int32_t field, double *out); /* bool/int/uint/f32/quantized; 1 = filled */
int32_t aurix_net_sync_get_vector(const AurixNetSync *sync, uint64_t entity, int32_t component, int32_t field, float *out4); /* 2 vec2, 3 vec3, 4 quat xyzw, 1 scalar, 0 missing */
int32_t aurix_net_sync_get_bytes(const AurixNetSync *sync, uint64_t entity, int32_t component, int32_t field, uint8_t *buf, size_t cap); /* string/bytes: full length (buf may be NULL), -1 missing */
int64_t aurix_net_sync_send_input(const AurixNetSync *sync, AurixNetClient *client, const uint8_t *data, size_t len); /* raw input for server logic; stamped tick or -1 */
int32_t aurix_net_sync_write_begin(const AurixNetSync *sync, int32_t component);           /* full component write, fields start at zero */
int32_t aurix_net_sync_write_number(const AurixNetSync *sync, int32_t field, double value);
int32_t aurix_net_sync_write_vector(const AurixNetSync *sync, int32_t field, const float *values, int32_t count); /* count = 2/3/4 */
int32_t aurix_net_sync_write_bytes(const AurixNetSync *sync, int32_t field, const uint8_t *data, size_t len);
int32_t aurix_net_sync_write_end(const AurixNetSync *sync);
int64_t aurix_net_sync_send_writes(const AurixNetSync *sync, AurixNetClient *client);           /* queued writes as this tick's input (owner-authoritative rooms); stamped tick or -1 */
int32_t aurix_net_sync_pump(const AurixNetSync *sync, AurixNetClient *client);                  /* sends inputs that were waiting for a tick and repeats the last frame on the next two; call after every applied delta */
int64_t aurix_net_sync_send_op(const AurixNetSync *sync, AurixNetClient *client, int32_t op, uint64_t entity, int32_t to); /* 0 write entity (queued writes), 1 spawn (queued writes), 2 despawn, 3 transfer to actor `to`, 4 set ownership policy `to` (0 destroy_on_leave, 1 to_master, 2 orphan, 3 dynamic), 5 request ownership, 6 release; stamped tick or -1 */
int64_t aurix_net_sync_spawn(const AurixNetSync *sync, AurixNetClient *client);                 /* spawn (queued writes) under a tag the room answers with the entity; repeated until answered; the tag or -1 (the room does not answer tagged spawns) */

#ifdef __cplusplus
}
#endif
#endif
