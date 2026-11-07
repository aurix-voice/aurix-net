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

#ifdef __cplusplus
}
#endif
#endif
