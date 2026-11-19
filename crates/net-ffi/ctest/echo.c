#define _DEFAULT_SOURCE 1 /* usleep under -std=c11 */
#include "../include/aurix_net.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    const char *http = "http://127.0.0.1:8090";
    const char *key = getenv("NET_API_KEY");
    if (!key) { fprintf(stderr, "NET_API_KEY missing\n"); return 2; }
    char *ga = aurix_net_issue_grant(http, key, "c-alice", "Alice");
    char *gb = aurix_net_issue_grant(http, key, "c-bob", "Bob");
    char *gc = aurix_net_issue_grant(http, key, "c-carol", "Carol");
    if (!ga || !gb || !gc) { fprintf(stderr, "grant: %s\n", aurix_net_last_error()); return 1; }
    AurixNetClient *a = aurix_net_connect(http, ga, 0);
    AurixNetClient *b = aurix_net_connect(http, gb, 1);
    AurixNetClient *c = aurix_net_connect(http, gc, 0);
    if (!a || !b || !c) { fprintf(stderr, "connect: %s\n", aurix_net_last_error()); return 1; }
    char join[256];
    /* a refused join says why in aurix_net_last_error_code() */
    snprintf(join, sizeof join, "{\"t\":\"join\",\"name\":\"c-missing-%d\"}", (int)getpid());
    if (aurix_net_join(a, join) >= 0 || aurix_net_last_error_code() != AURIX_NET_ERR_NOT_FOUND) { fprintf(stderr, "missing room: code %d (%s)\n", (int)aurix_net_last_error_code(), aurix_net_last_error()); return 1; }
    /* a message with a field the node does not know is refused before it is sent, and the field is named */
    snprintf(join, sizeof join, "{\"t\":\"join\",\"name\":\"c-missing-%d\",\"mode\":\"relay\"}", (int)getpid());
    if (aurix_net_join(a, join) >= 0 || aurix_net_last_error_code() != AURIX_NET_ERR_INVALID_REQUEST || !strstr(aurix_net_last_error(), "unknown field `mode`")) { fprintf(stderr, "unknown field: code %d (%s)\n", (int)aurix_net_last_error_code(), aurix_net_last_error()); return 1; }
    snprintf(join, sizeof join, "{\"t\":\"join_or_create\",\"name\":\"c-one-%d\",\"max_players\":1}", (int)getpid());
    if (aurix_net_join(a, join) < 0) { fprintf(stderr, "join: %s\n", aurix_net_last_error()); return 1; }
    if (aurix_net_join(b, join) >= 0 || aurix_net_last_error_code() != AURIX_NET_ERR_ROOM_FULL) { fprintf(stderr, "full room: code %d (%s)\n", (int)aurix_net_last_error_code(), aurix_net_last_error()); return 1; }
    aurix_net_leave(a);
    /* a closed room lets its creator in and nobody else */
    snprintf(join, sizeof join, "{\"t\":\"join_or_create\",\"name\":\"c-closed-%d\",\"open\":false}", (int)getpid());
    if (aurix_net_join(c, join) < 0) { fprintf(stderr, "closed room: %s\n", aurix_net_last_error()); return 1; }
    if (aurix_net_join(b, join) >= 0 || aurix_net_last_error_code() != AURIX_NET_ERR_ROOM_CLOSED) { fprintf(stderr, "join_or_create of a closed room: code %d (%s)\n", (int)aurix_net_last_error_code(), aurix_net_last_error()); return 1; }
    snprintf(join, sizeof join, "{\"t\":\"join\",\"name\":\"c-closed-%d\"}", (int)getpid());
    if (aurix_net_join(b, join) >= 0 || aurix_net_last_error_code() != AURIX_NET_ERR_ROOM_CLOSED) { fprintf(stderr, "join of a closed room: code %d (%s)\n", (int)aurix_net_last_error_code(), aurix_net_last_error()); return 1; }
    /* the room of the echo is hidden: a random join is not offered it, a join by its name enters it */
    snprintf(join, sizeof join, "{\"t\":\"join_or_create\",\"name\":\"c-echo-%d\",\"max_players\":4,\"visible\":false,\"props\":{\"map\":\"c-echo-%d\"},\"lobby_props\":[\"map\"]}", (int)getpid(), (int)getpid());
    int aa = aurix_net_join(a, join);
    if (aa < 0) { fprintf(stderr, "join: %s\n", aurix_net_last_error()); return 1; }
    char random[128];
    snprintf(random, sizeof random, "{\"t\":\"join_random\",\"expr\":\"map = 'c-echo-%d'\"}", (int)getpid());
    if (aurix_net_join(b, random) >= 0 || aurix_net_last_error_code() != AURIX_NET_ERR_NOT_FOUND) { fprintf(stderr, "random join of a hidden room: code %d (%s)\n", (int)aurix_net_last_error_code(), aurix_net_last_error()); return 1; }
    snprintf(join, sizeof join, "{\"t\":\"join\",\"name\":\"c-echo-%d\"}", (int)getpid());
    int ab = aurix_net_join(b, join);
    if (ab < 0) { fprintf(stderr, "hidden room by name: %s\n", aurix_net_last_error()); return 1; }
    char *jj = aurix_net_joined_json(b);
    printf("alice=%d bob=%d joined=%s\n", aa, ab, jj);
    aurix_net_string_free(jj);
    const uint8_t hello[] = "hello";
    aurix_net_send_event(a, 1, AURIX_NET_TARGET_OTHERS, NULL, 0, hello, 5, 1);
    for (uint8_t i = 0; i < 10; i++) { aurix_net_send_event(a, 2, AURIX_NET_TARGET_OTHERS, NULL, 0, &i, 1, 0); aurix_net_flush(a); usleep(16000); }
    int got_hello = 0, ticks = 0;
    for (int spin = 0; spin < 300 && !(got_hello && ticks == 10); spin++) {
        AurixNetEvent ev;
        while (aurix_net_poll(b, &ev)) {
            if (ev.kind == AURIX_NET_EVENT_DATA && ev.data_len > 0) {
                /* event frame: u8 code, u8 flags, u16 from, [group], payload */
                uint8_t code = ev.data[0];
                if (code == 1) got_hello = 1; else if (code == 2) ticks++;
            } else if (ev.kind == AURIX_NET_EVENT_CONTROL) {
                printf("control: %.80s\n", ev.json);
            }
            aurix_net_event_free(&ev);
        }
        usleep(10000);
    }
    printf("bob got hello=%d ticks=%d/10\n", got_hello, ticks);
    aurix_net_leave(a); aurix_net_destroy(a); aurix_net_destroy(b); aurix_net_destroy(c);
    aurix_net_string_free(ga); aurix_net_string_free(gb); aurix_net_string_free(gc);
    return (got_hello && ticks == 10) ? 0 : 1;
}
