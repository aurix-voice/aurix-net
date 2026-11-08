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

enum { AURIX_NET_CH_UNRELIABLE = 0, AURIX_NET_CH_RELIABLE = 1, AURIX_NET_CH_RELIABLE_UNORDERED = 2 };
enum { AURIX_NET_TARGET_ALL = 0, AURIX_NET_TARGET_OTHERS = 1, AURIX_NET_TARGET_MASTER = 2, AURIX_NET_TARGET_ACTORS = 3 };

typedef struct AurixNetEvent {
    int32_t kind;
    uint8_t channel;
    uint16_t tick;
    uint8_t *data;
    size_t data_len;
    char *json;
} AurixNetEvent;

const char *aurix_net_version(void);
const char *aurix_net_last_error(void);

/* backend/tooling only */
char *aurix_net_issue_grant(const char *http, const char *api_key, const char *player_id, const char *name);
void aurix_net_string_free(char *s);

AurixNetClient *aurix_net_connect(const char *http, const char *grant, int32_t tunnel);
int32_t aurix_net_send_control(AurixNetClient *h, const char *json);
int32_t aurix_net_join(AurixNetClient *h, const char *json);          /* returns actor id or -1 */
char *aurix_net_joined_json(AurixNetClient *h);
int32_t aurix_net_send_data(AurixNetClient *h, uint8_t channel, const uint8_t *data, size_t len);
int32_t aurix_net_send_event(AurixNetClient *h, uint8_t code, int32_t target, const uint16_t *actors, size_t n_actors,
                             const uint8_t *data, size_t len, int32_t reliable);
void aurix_net_flush(AurixNetClient *h);
int32_t aurix_net_poll(AurixNetClient *h, AurixNetEvent *out);          /* 1 = event filled */
void aurix_net_event_free(AurixNetEvent *ev);
int32_t aurix_net_leave(AurixNetClient *h);
void aurix_net_destroy(AurixNetClient *h);

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
int32_t aurix_net_lockstep_receive(AurixNetLockstep *ls, const uint8_t *data, size_t len); /* 1 = lockstep frame */
int32_t aurix_net_lockstep_poll(AurixNetLockstep *ls, AurixNetLsEvent *out);               /* 1 = event filled */
int32_t aurix_net_lockstep_input(AurixNetLockstep *ls, uint16_t index, uint16_t *actor, const uint8_t **data, size_t *len);
int64_t aurix_net_lockstep_send_input(AurixNetLockstep *ls, AurixNetClient *h, const uint8_t *data, size_t len); /* stamped tick or -1 */
int32_t aurix_net_lockstep_report_hash(AurixNetClient *h, uint32_t tick, uint64_t hash);
int32_t aurix_net_lockstep_send_checkpoint(AurixNetClient *h, uint32_t tick, uint64_t hash, const uint8_t *state, size_t len);
int32_t aurix_net_lockstep_send_dump(AurixNetClient *h, uint32_t tick, uint64_t hash, const uint8_t *state, size_t len);
int64_t aurix_net_lockstep_get(AurixNetLockstep *ls, int32_t key);
int32_t aurix_net_lockstep_set(AurixNetLockstep *ls, int32_t key, int64_t value);
uint64_t aurix_net_lockstep_fnv1a(const uint8_t *data, size_t len);

/* ---- sync rooms (client-side replicated world; feed it every data payload of a sync room) ---- */
typedef struct AurixNetSync AurixNetSync;
enum { AURIX_NET_SYNC_NONE = 0, AURIX_NET_SYNC_SPAWN = 1, AURIX_NET_SYNC_CHANGE = 2, AURIX_NET_SYNC_DESPAWN = 3 };
typedef struct AurixNetSyncEvent { int32_t kind; uint64_t entity; int32_t owner; /* -1 = server-owned */ } AurixNetSyncEvent;
AurixNetSync *aurix_net_sync_new(const char *schema);                                   /* NULL + last_error on a bad schema */
void aurix_net_sync_destroy(AurixNetSync *s);
int32_t aurix_net_sync_receive(const AurixNetSync *s, const uint8_t *data, size_t len, uint8_t *ack9); /* 1 = delta applied, ack9 filled (send it unreliable); 0 = not a delta; -1 = error */
int32_t aurix_net_sync_poll(const AurixNetSync *s, AurixNetSyncEvent *out);           /* 1 = spawn/change/despawn filled */
uint32_t aurix_net_sync_server_tick(const AurixNetSync *s);
uint64_t aurix_net_sync_gaps(const AurixNetSync *s);
int32_t aurix_net_sync_entity_count(const AurixNetSync *s);
int32_t aurix_net_sync_entity_at(const AurixNetSync *s, int32_t index, uint64_t *id, int32_t *owner); /* 1 = filled */
int32_t aurix_net_sync_component(const AurixNetSync *s, const char *name);              /* component id or -1 */
int32_t aurix_net_sync_field(const AurixNetSync *s, int32_t component, const char *name); /* field index or -1 */
int32_t aurix_net_sync_has(const AurixNetSync *s, uint64_t entity, int32_t component);
int32_t aurix_net_sync_get_number(const AurixNetSync *s, uint64_t entity, int32_t component, int32_t field, double *out); /* bool/int/uint/f32/quantized; 1 = filled */
int32_t aurix_net_sync_get_vector(const AurixNetSync *s, uint64_t entity, int32_t component, int32_t field, float *out4); /* 2 vec2, 3 vec3, 4 quat xyzw, 1 scalar, 0 missing */
int32_t aurix_net_sync_get_bytes(const AurixNetSync *s, uint64_t entity, int32_t component, int32_t field, uint8_t *buf, size_t cap); /* string/bytes: full length (buf may be NULL), -1 missing */
int64_t aurix_net_sync_send_input(const AurixNetSync *s, AurixNetClient *h, const uint8_t *data, size_t len); /* raw input for server logic; stamped tick or -1 */
int32_t aurix_net_sync_write_begin(const AurixNetSync *s, int32_t component);           /* full component write, fields start at zero */
int32_t aurix_net_sync_write_number(const AurixNetSync *s, int32_t field, double v);
int32_t aurix_net_sync_write_vector(const AurixNetSync *s, int32_t field, const float *v, int32_t n); /* n = 2/3/4 */
int32_t aurix_net_sync_write_bytes(const AurixNetSync *s, int32_t field, const uint8_t *p, size_t n);
int32_t aurix_net_sync_write_end(const AurixNetSync *s);
int64_t aurix_net_sync_send_writes(const AurixNetSync *s, AurixNetClient *h);           /* queued writes as this tick's input (owner-authoritative rooms); stamped tick or -1 */

#ifdef __cplusplus
}
#endif
#endif
