#!/usr/bin/env bash
# Live: Web (sdk/web/test/rehost.mjs) and C# (sdk/unity/BuildCheck~/managed-rehost) clients sit in
# one relay room on node A; A gets SIGTERM; both must receive left{reason:"rehost"}, rejoin on B
# with the same grant/actor and still reach a client that joins on B afterwards.
# Needs: `cargo build --release`, `npm run build` in sdk/web, `dotnet build -c Release` in the
# managed-rehost project, NET_API_KEY in the environment. Expects two-nodes.sh layout (A :8090, B :8091).
set -u
cd "$(dirname "$0")/.."
DOTNET="${DOTNET:-dotnet}"
scripts/two-nodes.sh stop >/dev/null 2>&1; sleep 1; scripts/two-nodes.sh start
ROOM="sdk-rehost-$(date +%s)"
(cd sdk/web && node test/rehost.mjs "$ROOM" > /tmp/web-rehost.log 2>&1; echo "web exit $?" >> /tmp/web-rehost.log) &
(cd "sdk/unity/BuildCheck~/managed-rehost" && "$DOTNET" run --no-build -c Release -- "$ROOM" > /tmp/cs-rehost.log 2>&1; echo "cs exit $?" >> /tmp/cs-rehost.log) &
sleep 8
kill -TERM "$(pgrep -f '^/tmp/aurix-net-A')"; echo "A: SIGTERM sent"
wait
echo "=== web"; cat /tmp/web-rehost.log; echo "=== cs"; cat /tmp/cs-rehost.log
echo "=== 'adopted orphaned room' on B:"; grep -c "adopted orphaned room" /tmp/aurix-net-B.log
echo "PASS criteria: WEB_REHOST_OK, CS_REHOST_OK, same actor: true, adopted >= 1"
