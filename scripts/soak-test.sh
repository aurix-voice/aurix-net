#!/usr/bin/env bash
# Soak: relay + sync bots against node B for DURATION seconds while sampling both nodes' live
# stats (RSS, rooms, players, sync tick avg/p99, overruns) every 60 s into a CSV. The point is
# flat RSS and a flat tick after the first minutes: no leak per room/player churn.
#   DURATION=3600 ./scripts/soak-test.sh
set -euo pipefail
A="${HTTP_A:-http://127.0.0.1:8098}"; B="${HTTP_B:-http://127.0.0.1:8099}"
TOK="${NET_OPERATOR_TOKEN:-dev-admin}"; DURATION="${DURATION:-3600}"; BATCH="${BATCH:-300}"
BIN="${LOADTEST:-$(dirname "$0")/../target/release/aurix-net-loadtest}"
OUT="${OUT:-/tmp/soak-$(date +%Y%m%d-%H%M%S).csv}"
auth=(-H "Authorization: Bearer $TOK")
ids=$(curl -sf "${auth[@]}" "$A/v1/operator/nodes" | python3 -c 'import sys,json; print(" ".join(n["id"] for n in json.load(sys.stdin)["nodes"]))')
echo "t,node,rss_mb,rooms,players,tick_avg_ms,tick_p99_ms,tick_overruns" > "$OUT"
sample() {
  for id in $ids; do
    curl -sf "${auth[@]}" "$A/v1/operator/nodes/$id/stats" | python3 -c '
import sys,json,time; s=json.load(sys.stdin)
print(",".join(str(x) for x in [int(time.time()),s["node_id"][-12:],s.get("rss_mb"),s.get("rooms"),s.get("players"),s.get("sync_tick_avg_ms"),s.get("sync_tick_p99_ms"),s.get("tick_overruns")]))' >> "$OUT" || true
  done
}
end=$(( $(date +%s) + DURATION ))
sample
(
  while [ "$(date +%s)" -lt "$end" ]; do
    "$BIN" --http "$B" --operator-token "$TOK" --rooms 2 --players 8 --rate 20 --duration-secs "$BATCH" --tunnel >/dev/null 2>&1 &
    "$BIN" --http "$B" --operator-token "$TOK" --rooms 1 --players 8 --rate 20 --mode sync --duration-secs "$BATCH" >/dev/null 2>&1 &
    wait
    sleep 5
  done
) &
bots=$!
while [ "$(date +%s)" -lt "$end" ]; do sleep 60; sample; done
wait "$bots" || true
sleep 20; sample
echo "samples in $OUT"
python3 - "$OUT" <<'PY'
import sys,csv,collections
rows=list(csv.DictReader(open(sys.argv[1])))
by=collections.defaultdict(list)
for r in rows: by[r["node"]].append(r)
for n,rs in by.items():
    rss=[float(r["rss_mb"]) for r in rs if r["rss_mb"] not in ("","None")]
    p99=[float(r["tick_p99_ms"]) for r in rs if r["tick_p99_ms"] not in ("","None")]
    ov=[int(float(r["tick_overruns"])) for r in rs if r["tick_overruns"] not in ("","None")]
    print(f"node {n}: samples={len(rs)} rss first/last/max={rss[0]:.0f}/{rss[-1]:.0f}/{max(rss):.0f} MB  tick p99 max={max(p99) if p99 else 0:.2f} ms  overruns={ov[-1]-ov[0] if ov else 0}")
PY
