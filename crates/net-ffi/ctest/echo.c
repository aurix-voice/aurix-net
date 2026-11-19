#define _DEFAULT_SOURCE 1 /* usleep under -std=c11 */
#include "../include/aurix_net.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* how many events of this code the client has got */
static int heard(AurixNetClient *client, uint8_t code) {
    int n = 0;
    AurixNetEvent ev;
    while (aurix_net_poll(client, &ev)) {
        if (ev.kind == AURIX_NET_EVENT_DATA && ev.data_len > 0 && ev.data[0] == code) n++;
        aurix_net_event_free(&ev);
    }
    return n;
}

/* what a client is told about the room it loses, in order: the events handed back, `left` and its reason, `closed` */
static void told_of(AurixNetClient *client, char *told, size_t cap) {
    AurixNetEvent ev;
    while (aurix_net_poll(client, &ev)) {
        size_t at = strlen(told);
        if (ev.kind == AURIX_NET_EVENT_UNDELIVERED) {
            for (size_t i = 0; i < aurix_net_undelivered_count(&ev); i++) {
                AurixNetUndelivered e;
                uint16_t actors[2] = {0, 0};
                if (aurix_net_undelivered_get(&ev, i, &e) < 0) continue;
                size_t n = aurix_net_undelivered_actors(&ev, i, actors, 2);
                at += (size_t)snprintf(told + at, cap - at, "%d:%zu:%d:%zu:%d+%d:%d:%d ", (int)e.code, e.len, (int)e.target, n, (int)actors[0], (int)actors[1], (int)e.group, (int)e.cache);
            }
            snprintf(told + at, cap - at, "| ");
        } else if (ev.kind == AURIX_NET_EVENT_CONTROL) {
            char *t = aurix_net_json_string(ev.json, "t");
            char *reason = aurix_net_json_string(ev.json, "reason");
            if (t && strcmp(t, "left") == 0) snprintf(told + at, cap - at, "left %s | ", reason ? reason : "");
            aurix_net_string_free(t); aurix_net_string_free(reason);
        } else if (ev.kind == AURIX_NET_EVENT_CLOSED) {
            snprintf(told + at, cap - at, "closed | ");
        }
        aurix_net_event_free(&ev);
    }
}

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
    /* an error carried over from another thread reads as the last one of this thread */
    aurix_net_set_last_error(AURIX_NET_ERR_TIMEOUT, "carried over");
    if (aurix_net_last_error_code() != AURIX_NET_ERR_TIMEOUT || strcmp(aurix_net_last_error(), "carried over") != 0) { fprintf(stderr, "set_last_error: code %d (%s)\n", (int)aurix_net_last_error_code(), aurix_net_last_error()); return 1; }
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
    /* over a link that takes 100 ms each way and closes on `stats` (NET_BREAK_HTTP, see sdk/cpp/README.md): the
     * reliable events the node did not acknowledge come before the room is lost, and not to a client that leaves */
    int handed_back = 1;
    const char *broken = getenv("NET_BREAK_HTTP");
    const char *hows[] = {"kick", "cut", "leave"};
    for (int h = 0; broken && h < 3; h++) {
        char id[32];
        snprintf(id, sizeof id, "c-m-%s", hows[h]);
        char *gm = aurix_net_issue_grant(http, key, id, "M");
        snprintf(id, sizeof id, "c-u-%s", hows[h]);
        char *gu = aurix_net_issue_grant(http, key, id, "U");
        AurixNetClient *m = aurix_net_connect(http, gm, 0);
        AurixNetClient *u = aurix_net_connect(broken, gu, 1);
        if (!m || !u) { fprintf(stderr, "broken link: %s\n", aurix_net_last_error()); return 1; }
        snprintf(join, sizeof join, "{\"t\":\"join_or_create\",\"name\":\"c-break-%d-%s\",\"max_players\":4}", (int)getpid(), hows[h]);
        if (aurix_net_join(m, join) < 0) { fprintf(stderr, "broken link: %s\n", aurix_net_last_error()); return 1; }
        snprintf(join, sizeof join, "{\"t\":\"join\",\"name\":\"c-break-%d-%s\"}", (int)getpid(), hows[h]);
        int actor = aurix_net_join(u, join);
        if (actor < 0) { fprintf(stderr, "broken link: %s\n", aurix_net_last_error()); return 1; }
        char told[256] = "";
        const uint8_t one = 1;
        static uint8_t big[3000];
        const uint16_t to[] = {7, 9};
        aurix_net_send_event(u, 1, AURIX_NET_TARGET_OTHERS, NULL, 0, &one, 1, 1);
        for (int spin = 0; spin < 300 && !heard(m, 1); spin++) { aurix_net_flush(u); told_of(u, told, sizeof told); usleep(10000); }
        for (int spin = 0; spin < 40; spin++) { told_of(u, told, sizeof told); usleep(10000); }
        aurix_net_send_event_ex(u, 2, AURIX_NET_TARGET_ACTORS, to, 2, &one, 1, 1, -1, 0);
        aurix_net_send_event(u, 3, AURIX_NET_TARGET_OTHERS, NULL, 0, &one, 1, 0);
        aurix_net_send_event_ex(u, 5, AURIX_NET_TARGET_MASTER, NULL, 0, big, sizeof big, 1, -1, 1);
        aurix_net_send_event_ex(u, 6, AURIX_NET_TARGET_ALL, NULL, 0, &one, 1, 1, 9, 0);
        aurix_net_flush(u);
        if (h == 0) {
            char kick[64];
            snprintf(kick, sizeof kick, "{\"t\":\"kick\",\"actor\":%d}", actor);
            aurix_net_send_control(m, kick);
        } else if (h == 1) {
            aurix_net_send_control(u, "{\"t\":\"stats\"}");
        } else {
            aurix_net_leave(u);
        }
        for (int spin = 0; spin < 100; spin++) { told_of(u, told, sizeof told); heard(m, 0); usleep(10000); }
        const char *want = h == 0 ? "2:1:3:2:7+9:-1:0 5:3000:2:0:0+0:-1:1 6:1:0:0:0+0:9:0 | left kicked | "
                         : h == 1 ? "2:1:3:2:7+9:-1:0 5:3000:2:0:0+0:-1:1 6:1:0:0:0+0:9:0 | closed | "
                                  : "left left | ";
        int ok = strcmp(told, want) == 0;
        printf("c undelivered events, %s: [%s] -> %s\n", hows[h], told, ok ? "ok" : "FAIL");
        handed_back = handed_back && ok;
        aurix_net_destroy(m); aurix_net_destroy(u);
        aurix_net_string_free(gm); aurix_net_string_free(gu);
    }
    if (!broken) {
        printf("c broken link: skipped (NET_BREAK_HTTP not set)\n");
        handed_back = !getenv("NET_LIVE_REQUIRED");
    }
    aurix_net_leave(a); aurix_net_destroy(a); aurix_net_destroy(b); aurix_net_destroy(c);
    aurix_net_string_free(ga); aurix_net_string_free(gb); aurix_net_string_free(gc);
    return (got_hello && ticks == 10 && handed_back) ? 0 : 1;
}
