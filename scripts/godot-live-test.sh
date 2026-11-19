#!/usr/bin/env bash
# Live: the Godot check project (sdk/godot/check~, see sdk/godot/README.md) against two nodes, all of
# it: relay, sync and lockstep rooms, the slow link, and a room handed from node A to node B. The
# check ends in a room on A and waits to be moved; A gets SIGTERM once that room is there.
# Needs: GODOT (a Godot 4.2+ binary), `cargo build --release -p net-ffi`, the extension built into
# sdk/godot/bin, jq, NET_API_KEY and NET_SLOW_HTTP (sdk/cpp/README.md) in the environment. Expects
# two-nodes.sh layout (A :8090, B :8091).
set -u
cd "$(dirname "$0")/.."
: "${GODOT:?}" "${NET_API_KEY:?}"
cp sdk/godot/bin/*.so target/release/libaurix_net.so "sdk/godot/check~/addons/aurix_net/bin/" || exit 1
scripts/two-nodes.sh stop >/dev/null 2>&1; scripts/two-nodes.sh start
grant() {
  curl -fs -H "Authorization: Bearer $NET_API_KEY" -H 'content-type: application/json' \
    -d "{\"player_id\":\"godot-$1\",\"name\":\"godot-$1\"}" http://127.0.0.1:8090/v1/grants | jq -r .grant
}
for p in A B C D E F G H; do export "GRANT_$p=$(grant "$p")"; done
export NET_HTTP=http://127.0.0.1:8090 NET_REHOST_HTTP=http://127.0.0.1:8091 NET_REHOST_ROOM="godot-rehost-$(date +%s)"
"$GODOT" --headless --path "sdk/godot/check~" > /tmp/godot-live.log 2>&1 & check=$!
rooms() { curl -fs -H "Authorization: Bearer $NET_API_KEY" http://127.0.0.1:8090/v1/rooms; }
while kill -0 "$check" 2>/dev/null && ! rooms | grep -q "$NET_REHOST_ROOM"; do sleep 1; done
sleep 3
kill -TERM "$(pgrep -f '^/tmp/aurix-net-A')"; echo "A: SIGTERM sent"
wait "$check"; rc=$?
cat /tmp/godot-live.log
exit $rc
