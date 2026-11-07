#!/usr/bin/env bash
# Two local nodes sharing the dev Postgres/Redis (deploy/docker-compose.dev.yml):
#   A: http 8090, quic 7000, webtransport 7443     B: http 8091, quic 7001, webtransport 7444
# Usage: scripts/two-nodes.sh start|stop   (binaries from `cargo build --release`)
set -euo pipefail
BIN="$(dirname "$0")/../target/release/aurix-net-server"
export NET__AUTH__ADMIN_TOKEN="${NET__AUTH__ADMIN_TOKEN:-dev-admin}" NET__SERVER__HEARTBEAT_SECS="${HEARTBEAT:-3}" RUST_LOG="${RUST_LOG:-info}"
case "${1:-start}" in
  start)
    ln -sf "$(readlink -f "$BIN")" /tmp/aurix-net-A; ln -sf "$(readlink -f "$BIN")" /tmp/aurix-net-B
    /tmp/aurix-net-A >/tmp/aurix-net-A.log 2>&1 &
    NET__SERVER__HTTP_ADDR=127.0.0.1:8091 NET__TRANSPORT__QUIC_ADDR=0.0.0.0:7001 NET__TRANSPORT__WEBTRANSPORT_ADDR=0.0.0.0:7444 \
      /tmp/aurix-net-B >/tmp/aurix-net-B.log 2>&1 &
    for p in 8090 8091; do for _ in $(seq 1 30); do curl -sf 127.0.0.1:$p/healthz >/dev/null && break; sleep 1; done; done
    echo "A and B up (logs /tmp/aurix-net-{A,B}.log)";;
  stop) pkill -f '^/tmp/aurix-net-[AB]' || true;;
  kill-a) kill -9 "$(pgrep -f '^/tmp/aurix-net-A')";;
esac
