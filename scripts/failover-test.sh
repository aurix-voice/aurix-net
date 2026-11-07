#!/usr/bin/env bash
# Chaos: a lockstep match runs on node A, A is killed mid-match, players reconnect to node B,
# which must adopt the room from the Redis checkpoint and serve catch-up. Expects two-nodes.sh
# started and NET_API_KEY (rooms scope) in the environment.
# KILL_SIGNAL=9 (default) simulates a crash: B waits for 3 missed heartbeats before the room is
# orphaned. KILL_SIGNAL=TERM is a rolling deploy: A checkpoints and orphans its rooms itself, so
# the handoff is immediate.
set -euo pipefail
LT="$(dirname "$0")/../target/release/aurix-net-loadtest"
RUN=$(date +%s)
"$LT" --api-key "$NET_API_KEY" --mode lockstep --rooms 1 --players 3 --duration-secs 40 --run "$RUN" >/tmp/failover-a.log 2>&1 &
sleep 14
SIG="${KILL_SIGNAL:-9}"
kill -"$SIG" "$(pgrep -f '^/tmp/aurix-net-A')"; echo "A killed (signal $SIG)"
if [ "$SIG" = "9" ]; then sleep 14; else sleep 2; fi   # crash: 3 missed heartbeats + reap; TERM: handoff is immediate
"$LT" --http http://127.0.0.1:8091 --api-key "$NET_API_KEY" --mode lockstep --rooms 1 --players 2 --duration-secs 14 --run "$RUN" --late-join-secs 1 2>&1 | grep -E "^joined=|late join|catch-up gap|still catching"
grep -c "adopted orphaned room" /tmp/aurix-net-B.log
echo "PASS criteria: joined=3 failed=0, caught-up=3/1, 'adopted orphaned room' >= 1"
