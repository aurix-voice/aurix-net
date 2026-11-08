# Self-host release checklist

What to do, in order, to put an Aurix Net deployment into production or roll a new version out.
Each step points at the document that explains it; nothing here is optional unless marked.

## Before the first deploy

1. **Secrets** — generate `GRANT_SECRET` (≥ 32 random bytes), `ADMIN_TOKEN`, `METRICS_TOKEN`,
   `PG_PASSWORD`, `REDIS_PASSWORD` (`openssl rand -hex 32`) into a `.env` with mode 600, outside
   the repo. The node refuses to start in `production` with a short or `dev-only` grant secret
   ([deployment.md → Configuration](deployment.md#configuration)).
2. **Network** — Postgres/Redis on the internal network only; TLS terminated in front of
   `8090/tcp` (`deploy/Caddyfile.example`); `7000/udp` + `7443/udp` straight to the nodes with
   their own certificate; proxy ranges in `server.trusted_proxies`; `/metrics` behind
   `METRICS_TOKEN` ([Network](deployment.md#network)).
3. **Image** — tag the commit whose `CHANGELOG.md` has the entry and whose version fields match
   (`Cargo.toml` workspace, `sdk/web`, `sdk/unity`, `dashboard` package.json); build
   `aurix-net:<tag>` from it (`docker build .`), scan it
   (`trivy image aurix-net:<tag>`; CI fails on CRITICAL with a fix), keep the `sbom` artifact
   of that CI run with the release.
4. **Compose** — copy `deploy/docker-compose.prod.yml`, set `PUBLIC_HOST` per node and
   `REGION`, point the console's `NET_API_URL` at the load balancer, keep `read_only`,
   `cap_drop`, `no-new-privileges`, `nofile`, log rotation and `stop_grace_period: 30s`.
5. **Migrate** — `docker compose run --rm node-a --migrate-only` once, then `up -d`
   ([Migrations](deployment.md#migrations)).
6. **Verify** — `/healthz` 200 on every node and via the proxy; `GET /v1/operator/nodes` lists
   all nodes with the right region; create a project + key, create a room with REST, join it with
   an SDK client through the proxy (WebSocket) and natively (QUIC); open the console.
7. **Monitoring** — `--profile monitoring up -d` or scrape `/metrics` from your Prometheus;
   load `deploy/monitoring/alerts.yml` (node down, slow ticks, overruns, desyncs, auth brute
   force, Redis write errors, rooms near cap) ([Monitoring](deployment.md#monitoring)).
8. **Backups** — schedule `scripts/backup.sh` (cron, off-host copy) and run one restore drill
   into a scratch database before go-live ([Backups](deployment.md#backups)).

## Every release

1. Read the release notes for config changes; nodes of one deployment run the same minor version.
2. Build + scan the image; `--migrate-only` with the new image while the old nodes keep serving.
   Exception, once: upgrading across migration `0012` (adds `nodes.heartbeat_secs`) — restart the old nodes
   right after migrating; their node-registry queries used `SELECT *` and fail with
   `cached plan must not change result type` until reconnect. From 0012 on these queries name their columns.
3. Rolling restart one node at a time, waiting for `/healthz` 200 before the next
   ([Upgrades](deployment.md#upgrades)); rooms are checkpointed and adopted by the other nodes,
   players reconnect by room name.
4. Watch `aurix_sync_tick_p99`, overruns, desync and auth-rejection counters for 15 minutes.
5. Roll back by redeploying the previous image; schema changes are additive, so the previous
   version runs against the migrated database. Restore from backup only if data was corrupted.

## Periodically

- Rotate secrets per the table in [Secrets and rotation](deployment.md#secrets-and-rotation);
  grant-secret rotation is zero-downtime via `auth.grant_secret_previous`.
- Rebuild the image when the distroless base or Rust dependencies get security updates
  (`cargo audit` / `cargo deny` run in CI on every push).
- Review `GET /v1/operator/audit` and per-project limits/quotas; prune expired API keys.
- Run a restore drill from a real backup.
