#!/usr/bin/env bash
# Consistent backup of a self-hosted Aurix Net deployment (Postgres + Redis) running under
# deploy/docker-compose.prod.yml. Postgres holds everything durable (projects, keys, rooms
# history, usage, audit, modules); Redis holds live state (room index, checkpoints of orphaned
# rooms, rate limits) — restoring it is optional but lets in-flight rooms survive a full restart.
#
#   scripts/backup.sh [out-dir]            # writes <out-dir>/aurix-net-<utc-stamp>/{postgres.sql.gz,redis.rdb,manifest}
#   set -a; . .env; set +a   # REDIS_PASSWORD for redis-cli
#   COMPOSE="docker compose -f deploy/docker-compose.prod.yml --env-file .env -p aurix" scripts/backup.sh
#
# Restore (stop the nodes first so they do not write while you load):
#   $COMPOSE stop node-a node-b
#   gunzip -c postgres.sql.gz | $COMPOSE exec -T postgres psql -U aurix -d aurix_net
#   $COMPOSE stop redis && docker cp redis.rdb <redis container>:/data/dump.rdb && $COMPOSE start redis
#   $COMPOSE start node-a node-b
set -euo pipefail
OUT=${1:-./backups}
COMPOSE=${COMPOSE:-"docker compose -f $(dirname "$0")/../deploy/docker-compose.prod.yml"}
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
DIR="$OUT/aurix-net-$STAMP"
mkdir -p "$DIR"
chmod 700 "$DIR"

# Postgres: single transaction snapshot, schema + data, plain SQL so it restores into any 16.x.
$COMPOSE exec -T postgres pg_dump -U aurix -d aurix_net --no-owner --no-privileges \
  | gzip -9 > "$DIR/postgres.sql.gz"

# Redis: ask for a point-in-time RDB and wait for it to land, then copy it out. redis-server
# rewrites its argv, so the password cannot be read back from the container: pass REDIS_PASSWORD
# (e.g. `set -a; . .env; set +a` before running) — unset means an unauthenticated Redis.
rcli() { $COMPOSE exec -T -e REDIS_PASSWORD="${REDIS_PASSWORD:-}" redis sh -c "redis-cli --no-auth-warning \${REDIS_PASSWORD:+-a \"\$REDIS_PASSWORD\"} $*"; }
[ "$(rcli PING)" = "PONG" ] || { echo "redis: cannot authenticate (set REDIS_PASSWORD)" >&2; exit 1; }
LAST=$(rcli LASTSAVE)
rcli BGSAVE >/dev/null
for _ in $(seq 1 60); do
  [ "$(rcli LASTSAVE)" != "$LAST" ] && break
  sleep 1
done
$COMPOSE exec -T redis cat /data/dump.rdb > "$DIR/redis.rdb"

{
  echo "created_utc=$STAMP"
  echo "postgres_sha256=$(sha256sum "$DIR/postgres.sql.gz" | cut -d' ' -f1)"
  echo "redis_sha256=$(sha256sum "$DIR/redis.rdb" | cut -d' ' -f1)"
  echo "postgres_bytes=$(stat -c %s "$DIR/postgres.sql.gz")"
  echo "redis_bytes=$(stat -c %s "$DIR/redis.rdb")"
} > "$DIR/manifest"
echo "backup written to $DIR"
cat "$DIR/manifest"
