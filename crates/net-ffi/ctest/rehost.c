/* Live rehost check (run by scripts/sdk-rehost-test.sh): two nodes and NET_API_KEY. Joins a relay
 * room on node A; a client that asks node B for that room is sent to A. Then waits for
 * left{reason:"rehost"} (A got SIGTERM) and joins the room again through node B with the same grant. */
#define _DEFAULT_SOURCE 1 /* usleep under -std=c11 */
#include "../include/aurix_net.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    const char *key = getenv("NET_API_KEY");
    if (!key || argc < 2) { fprintf(stderr, "usage: NET_API_KEY=... rehost <room>\n"); return 2; }
    char *grant = aurix_net_issue_grant("http://127.0.0.1:8090", key, "c-rehost-a", "A");
    AurixNetClient *a = grant ? aurix_net_connect("http://127.0.0.1:8090", grant, 0) : NULL;
    if (!a) { fprintf(stderr, "connect: %s\n", aurix_net_last_error()); return 1; }
    char join[256];
    snprintf(join, sizeof join, "{\"t\":\"join_or_create\",\"name\":\"%s\",\"max_players\":8}", argv[1]);
    int actor = aurix_net_join(a, join);
    if (actor < 0) { fprintf(stderr, "join: %s\n", aurix_net_last_error()); return 1; }
    printf("c: joined %s actor=%d on A\n", argv[1], actor);
    char *other_grant = aurix_net_issue_grant("http://127.0.0.1:8091", key, "c-rehost-c", "C");
    snprintf(join, sizeof join, "{\"t\":\"join\",\"name\":\"%s\"}", argv[1]);
    int there = -1;
    AurixNetClient *other = other_grant ? aurix_net_connect_and_join("http://127.0.0.1:8091", other_grant, 0, join, &there) : NULL;
    if (!other || there < 0 || there == actor) { printf("C_REHOST_FAIL asked B for the room on A: %s\n", other ? "no actor" : aurix_net_last_error()); return 1; }
    printf("c: asked B for the room on A: actor=%d\n", there);
    aurix_net_leave(other); aurix_net_destroy(other); aurix_net_string_free(other_grant);
    int rehost = 0;
    for (int spin = 0; spin < 6000 && !rehost; spin++) {
        AurixNetEvent ev;
        while (aurix_net_poll(a, &ev)) {
            if (ev.kind == AURIX_NET_EVENT_CONTROL && strstr(ev.json, "\"t\":\"left\"") && strstr(ev.json, "\"reason\":\"rehost\"")) rehost = 1;
            aurix_net_event_free(&ev);
        }
        usleep(10000);
    }
    if (!rehost) { printf("C_REHOST_FAIL no rehost within 60 s\n"); return 1; }
    char *described = aurix_net_joined_json(a);
    if (described) { printf("C_REHOST_FAIL still describes the room the node handed off\n"); return 1; }
    aurix_net_destroy(a);
    a = aurix_net_connect("http://127.0.0.1:8091", grant, 0);
    if (!a) { fprintf(stderr, "connect to B: %s\n", aurix_net_last_error()); return 1; }
    snprintf(join, sizeof join, "{\"t\":\"join\",\"name\":\"%s\"}", argv[1]);
    int again = aurix_net_join(a, join);
    printf("c: rejoined actor=%d (same actor: %s)\n", again, again == actor ? "true" : "false");
    printf(again == actor ? "C_REHOST_OK\n" : "C_REHOST_FAIL\n");
    aurix_net_destroy(a); aurix_net_string_free(grant);
    return again == actor ? 0 : 1;
}
