#!/usr/bin/env bash
# Capacity steps against one node pinned to a few cores: for each "rooms x players @ rate" step run the
# loadtest for STEP_SECS, then print node CPU (cores busy, from /proc), RSS, sync tick p99/overruns and
# the bots' delivery/latency numbers as one CSV row. Bots run on the other cores.
#   NODE_PID=<pid> HTTP=http://127.0.0.1:8098 NET_OPERATOR_TOKEN=dev-admin MODE=relay \
#   STEPS="10x8@20 20x8@20 40x8@20" BOT_CPUS=2-7 scripts/capacity.sh
set -euo pipefail
HTTP="${HTTP:-http://127.0.0.1:8098}"; TOK="${NET_OPERATOR_TOKEN:-dev-admin}"; MODE="${MODE:-relay}"
STEP_SECS="${STEP_SECS:-30}"; RAW="${RAW:-}"; BOT_CPUS="${BOT_CPUS:-2-7}"; EXTRA="${EXTRA:-}"
LT="${LOADTEST:-$(dirname "$0")/../target/release/aurix-net-loadtest}"
CLI="${CLI:-$(dirname "$0")/../target/release/aurix-net}"
: "${NODE_PID:?NODE_PID=<node pid>}"
KEY="${NET_API_KEY:-$(NET_HTTP="$HTTP" NET_OPERATOR_TOKEN="$TOK" "$CLI" project-create "cap-$(date +%s)" | grep -oE 'an_[A-Za-z0-9_-]+' | head -1)}"
TICK=$(getconf CLK_TCK)
cpu_ticks() { awk '{print $14+$15}' "/proc/$NODE_PID/stat"; }
rss_mb() { awk '/VmRSS/{printf "%.0f", $2/1024}' "/proc/$NODE_PID/status"; }
node_stat() { curl -s -H "Authorization: Bearer $TOK" "$HTTP/v1/operator/nodes" | python3 -c '
import json,sys; n=[x for x in json.load(sys.stdin)["nodes"] if x["http_url"].endswith(sys.argv[1])][0]; print(n["id"])' "${HTTP##*:}"; }
NODE_ID=$(node_stat)
stats() { curl -s -H "Authorization: Bearer $TOK" "$HTTP/v1/operator/nodes/$NODE_ID/stats" | python3 -c '
import json,sys; s=json.load(sys.stdin); print(s.get("sync_tick_p99_ms","") , s.get("tick_overruns",""), s.get("rooms",""), s.get("players",""))'; }
echo "mode,rooms,players,rate,joined,failed,loss_pct,p50_ms,p99_ms,node_cores,rss_mb,tick_p99_ms,overruns"
for st in $STEPS; do
  rooms=${st%%x*}; rest=${st#*x}; players=${rest%%@*}; rate=${rest#*@}
  t0=$(cpu_ticks); s0=$(date +%s.%N); ov0=$(stats | awk '{print $2}')
  out=$(taskset -c "$BOT_CPUS" "$LT" --http "$HTTP" --api-key "$KEY" --mode "$MODE" --rooms "$rooms" --players "$players" --rate "$rate" --duration-secs "$STEP_SECS" --run "$RANDOM" $EXTRA 2>&1 | tail -6)
  t1=$(cpu_ticks); s1=$(date +%s.%N)
  cores=$(python3 -c "print(round(($t1-$t0)/$TICK/($s1-$s0),2))")
  g() { echo "$out" | grep -oE "$1" | tail -1 | cut -d= -f2 || true; }
  joined=$(g 'joined=[0-9]+'); failed=$(g 'failed=[0-9]+'); loss=$(g 'loss=[0-9.]+'); p50=$(g 'p50=[0-9]+'); p99=$(g 'p99=[0-9]+')
  [ -n "$RAW" ] && printf '%s\n--- %s %s\n' "$out" "$MODE" "$st" >> "$RAW" || true
  read -r tp99 ov _ < <(stats); sleep 3
  echo "$MODE,$rooms,$players,$rate,${joined:-},${failed:-},${loss:-},$(python3 -c "print(round(${p50:-0}/1000,2))"),$(python3 -c "print(round(${p99:-0}/1000,2))"),$cores,$(rss_mb),${tp99},$(( ${ov:-0} - ${ov0:-0} ))"
  sleep 5
done
