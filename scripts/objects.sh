#!/usr/bin/env bash
# Bandwidth per client for N moving objects in sync rooms: for each "rooms x players @ objects" step
# run the loadtest with the `movers` logic (crates/net-game/examples/movers.wat, uploaded here) and
# print kB/s per client, objects per delta, node CPU (cores busy, from /proc), RSS and the node's tick
# time as one CSV row. CPU, tick rate and wire counters are taken after WARMUP_SECS. Bots run on
# BOT_CPUS, the node on the cores it was started on. A step waits until the node has no rooms left.
# node_steal is steal / (busy + steal) of /proc/stat on the node's cores: the share of the time those
# cores were wanted that the hypervisor gave to someone else. The kernel counts that time as the
# node's CPU, so a step with node_steal above a few percent reads node_cores too high and may tick
# too slowly: repeat it on cores without steal. With a mostly idle node (one room) the ratio is noise.
# ticks_s is ticks per room per second, udp_drops the kernel's Udp RcvbufErrors, lost_deltas the
# node's net_sync_lost_deltas_total, all inside the measured window.
#   NODE_PID=<pid> NET_API_KEY=an_... STEPS="1x16@100 1x16@250 1x16@500" scripts/objects.sh
# BUDGET=<bytes> sets the room's budget_bytes (default: the node's 1100). WIRE=1 also counts UDP
# packets of the data port on loopback with nftables (needs sudo): IP + UDP + QUIC included.
set -euo pipefail
HTTP="${HTTP:-http://127.0.0.1:8090}"; DATA_PORT="${DATA_PORT:-7000}"; TICK_HZ="${TICK_HZ:-20}"
STEP_SECS="${STEP_SECS:-40}"; WARMUP_SECS="${WARMUP_SECS:-10}"; BOT_CPUS="${BOT_CPUS:-2-7}"
BUDGET="${BUDGET:-}"; WIRE="${WIRE:-}"; RAW="${RAW:-}"
LT="${LOADTEST:-$(dirname "$0")/../target/release/aurix-net-loadtest}"
CLI="${CLI:-$(dirname "$0")/../target/release/aurix-net}"
: "${NODE_PID:?NODE_PID=<node pid>}" "${NET_API_KEY:?NET_API_KEY=<project key>}" "${STEPS:?STEPS=\"1x16@100 ...\"}"
NET_HTTP="$HTTP" "$CLI" module-put movers "$(dirname "$0")/../crates/net-game/examples/movers.wat" > /dev/null
PROPS=$(python3 -c '
import json,sys
p={"schema":"component Transform {\n pos: vec3 @range(-4096, 4096, 0.01)\n yaw: f32 @range(0, 360, 0.1)\n}","logic":"movers","aoi_cell":0}
if sys.argv[1]: p["budget_bytes"]=int(sys.argv[1])
print(json.dumps(p))' "$BUDGET")
if [ -n "$WIRE" ]; then
  sudo nft delete table inet aurix_objects 2> /dev/null || true
  sudo nft -f - <<NFT
table inet aurix_objects {
  counter down {}
  counter up {}
  chain out { type filter hook output priority 0;
    oifname "lo" udp sport $DATA_PORT counter name down
    oifname "lo" udp dport $DATA_PORT counter name up
  }
}
NFT
  trap 'sudo nft delete table inet aurix_objects' EXIT
fi
TICK=$(getconf CLK_TCK)
cpu_ticks() { awk '{print $14+$15}' "/proc/$NODE_PID/stat"; }
NODE_CPUS=$(taskset -cp "$NODE_PID" | sed 's/.*: //' | python3 -c '
import sys
print("|".join(str(c) for r in sys.stdin.read().strip().split(",") for a, _, b in [r.partition("-")] for c in range(int(a), int(b or a) + 1)))')
cores() { awk -v re="^cpu($NODE_CPUS)\$" '$1~re{b+=$2+$3+$4+$7+$8; s+=$9} END{print b, s}' /proc/stat; }
wire() { [ -n "$WIRE" ] && sudo nft list counter inet aurix_objects "$1" | grep -oE 'bytes [0-9]+' | cut -d' ' -f2 || echo 0; }
metric() { curl -s "$HTTP/metrics" | awk -v k="$1" '$1==k{print $2}'; }
udp_drops() { awk '/^Udp: [0-9]/{print $6}' /proc/net/snmp; }
echo "rooms,players,objects,budget,joined,failed,objects_on_client,down_kbs,up_kbs,wire_down_kbs,wire_up_kbs,deltas_s,objects_per_delta,node_cores,node_steal,ticks_s,udp_drops,lost_deltas,rss_mb,tick_mean_ms,client_gaps"
for st in $STEPS; do
  rooms=${st%%x*}; rest=${st#*x}; players=${rest%%@*}; objects=${rest#*@}; log=$(mktemp)
  # Players of the previous step stay "offline" for player_ttl_secs and their rooms keep ticking.
  for _ in $(seq 90); do [ "$(metric net_rooms)" = 0 ] && break; sleep 1; done
  [ "$(metric net_rooms)" = 0 ] || { echo "rooms left on the node, not measuring $st" >&2; exit 1; }
  ts0=$(metric net_sync_tick_seconds_sum); tc0=$(metric net_sync_tick_seconds_count)
  taskset -c "$BOT_CPUS" "$LT" --http "$HTTP" --api-key "$NET_API_KEY" --mode sync --rooms "$rooms" --players "$players" \
    --tick-hz "$TICK_HZ" --objects "$objects" --duration-secs "$STEP_SECS" --warmup-secs "$WARMUP_SECS" \
    --run "$RANDOM" --props-json "$PROPS" > "$log" 2>&1 &
  bots=$!
  sleep $((WARMUP_SECS + 1)); t0=$(cpu_ticks); c0=$(cores); k0=$(metric net_sync_tick_seconds_count); r0=$(udp_drops); l0=$(metric net_sync_lost_deltas_total); d0=$(wire down); u0=$(wire up); s0=$(date +%s.%N)
  sleep $((STEP_SECS - WARMUP_SECS - 2)); t1=$(cpu_ticks); c1=$(cores); k1=$(metric net_sync_tick_seconds_count); r1=$(udp_drops); l1=$(metric net_sync_lost_deltas_total); d1=$(wire down); u1=$(wire up); s1=$(date +%s.%N)
  rss=$(awk '/VmRSS/{printf "%.0f", $2/1024}' "/proc/$NODE_PID/status")
  wait "$bots" || true
  ts1=$(metric net_sync_tick_seconds_sum); tc1=$(metric net_sync_tick_seconds_count)
  [ -n "$RAW" ] && { tail -8 "$log"; printf -- '--- %s budget=%s\n' "$st" "${BUDGET:-default}"; } >> "$RAW" || true
  g() { grep -oE "$1" "$log" | tail -1 | cut -d= -f2 || true; }
  n=$((rooms * players))
  w() { [ -n "$WIRE" ] && python3 -c "print(round(($2-$1)/($s1-$s0)/$n/1000,2))" || true; }
  echo "$rooms,$players,$objects,${BUDGET:-default},$(g 'joined=[0-9]+'),$(g 'failed=[0-9]+'),$(g 'on client=[0-9]+'),$(g 'down=[0-9.]+'),$(g 'up=[0-9.]+'),$(w "$d0" "$d1"),$(w "$u0" "$u1"),$(g 'deltas=[0-9.]+'),$(g 'objects per delta=[0-9.]+'),$(python3 -c "print(round(($t1-$t0)/$TICK/($s1-$s0),2))"),$(python3 -c "
b0, s0, b1, s1 = map(float, '$c0 $c1'.split()); print(round((s1 - s0) / max(b1 - b0 + s1 - s0, 1), 3))"),$(python3 -c "print(round((${k1:-0}-${k0:-0})/($s1-$s0)/$rooms,1))"),$((r1 - r0)),$((l1 - l0)),$rss,$(python3 -c "print(round((${ts1:-0}-${ts0:-0})/max(${tc1:-0}-${tc0:-0},1)*1000,2))"),$(g 'gaps noticed=[0-9]+')"
  rm -f "$log"
done
