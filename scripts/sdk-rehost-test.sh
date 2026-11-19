#!/usr/bin/env bash
# Live: Web (sdk/web/test/rehost.mjs) and C# (sdk/unity/BuildCheck~/managed-rehost) clients sit in
# one relay room on node A, a C++ client (sdk/cpp/rehost_test.cpp) and a C client
# (crates/net-ffi/ctest/rehost.c) in rooms of their own; A gets SIGTERM; each must receive
# left{reason:"rehost"} and rejoin on B with the same grant/actor; the Web, C# and C++ ones must
# still reach a client that joins on B afterwards.
# Needs: `cargo build --release`, `npm run build` in sdk/web, `dotnet build -c Release` in the
# managed-rehost project, NET_API_KEY in the environment. Expects two-nodes.sh layout (A :8090, B :8091).
set -u
cd "$(dirname "$0")/.."
DOTNET="${DOTNET:-dotnet}"
g++ -std=c++17 sdk/cpp/rehost_test.cpp -Isdk/cpp -Ltarget/release -laurix_net -Wl,-rpath,"$PWD/target/release" -o /tmp/cpp-rehost || exit 1
gcc -std=c11 -Wall -Wextra -Werror crates/net-ffi/ctest/rehost.c -Ltarget/release -laurix_net -Wl,-rpath,"$PWD/target/release" -o /tmp/c-rehost || exit 1
scripts/two-nodes.sh stop >/dev/null 2>&1; scripts/two-nodes.sh start
ROOM="sdk-rehost-$(date +%s)"
(cd sdk/web && node test/rehost.mjs "$ROOM" > /tmp/web-rehost.log 2>&1; echo "web exit $?" >> /tmp/web-rehost.log) &
(/tmp/cpp-rehost "$ROOM-cpp" > /tmp/cpp-rehost.log 2>&1; echo "cpp exit $?" >> /tmp/cpp-rehost.log) &
(/tmp/c-rehost "$ROOM-c" > /tmp/c-rehost.log 2>&1; echo "c exit $?" >> /tmp/c-rehost.log) &
(cd "sdk/unity/BuildCheck~/managed-rehost" && "$DOTNET" run --no-build -c Release -- "$ROOM" > /tmp/cs-rehost.log 2>&1; echo "cs exit $?" >> /tmp/cs-rehost.log) &
sleep 8
kill -TERM "$(pgrep -f '^/tmp/aurix-net-A')"; echo "A: SIGTERM sent"
wait
echo "=== web"; cat /tmp/web-rehost.log; echo "=== cs"; cat /tmp/cs-rehost.log; echo "=== cpp"; cat /tmp/cpp-rehost.log; echo "=== c"; cat /tmp/c-rehost.log
echo "=== 'adopted orphaned room' on B:"; grep -c "adopted orphaned room" /tmp/aurix-net-B.log
echo "PASS criteria: WEB_REHOST_OK, CS_REHOST_OK, CPP_REHOST_OK, C_REHOST_OK, same actor: true, adopted >= 1"
