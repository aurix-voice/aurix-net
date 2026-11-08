# Deploying a node (production checklist)

## Network

- Postgres and Redis on a private network only, with authentication (`DATABASE_URL`
  with a strong password and `sslmode=require`; Redis `requirepass`/ACL via `redis://:password@host`, TLS via `rediss://` (built in), or a private
  link). The node never exposes them.
- Public ports: `server.http_addr` (TCP, control plane + WS tunnel), `transport.quic_addr`
  (UDP), `transport.webtransport_addr` (UDP). Everything else closed.
- Terminate TLS for the HTTP port at a reverse proxy or load balancer (the node speaks plain
  HTTP there; `deploy/Caddyfile.example` is a working two-node Caddy config with health-checked
  round robin that drops draining nodes) and put the proxy's address range into `server.trusted_proxies` so per-IP limits
  see real client addresses via `X-Forwarded-For`. QUIC/WebTransport use the node's own
  certificate (`transport.cert_pem`/`key_pem`); browsers need a publicly trusted one.
- Keep `/metrics` off the public interface or set `server.metrics_token`.

## Configuration

```toml
[server]
environment = "production"      # enables the secret checks below
region = "eu-central"
public_host = "node-1.eu.example.com"
trusted_proxies = ["10.0.0.0/8"]
metrics_token = "<random>"

[auth]
grant_secret = "<32+ random bytes>"   # shared by all nodes of a deployment
admin_token = "<random>"              # or empty to disable operator endpoints

[transport]
cert_pem = "..."                      # or a path via NET__TRANSPORT__CERT_PEM
key_pem = "..."
```

Everything can also come from the environment (`NET__AUTH__GRANT_SECRET=...`). Secrets must
not be baked into images or committed; use your orchestrator's secret store.

## Process

- Run as an unprivileged user; raise `nofile` (one fd per connection, ≥ 65536).
- Send `SIGTERM` to stop. The node marks itself draining (new joins refused, `/healthz` 503),
  checkpoints every room to Redis and marks them `orphaned`; the next node a player reaches
  adopts the room. Give it ≥ 10 s before `SIGKILL`. Rolling deploys with ≥ 2 nodes per region
  therefore do not end matches.
- Readiness probe: `GET /healthz` (200 = serving, 503 = draining). Liveness: TCP on the HTTP
  port.

## Limits worth tuning

`limits.*` in the config are node-wide ceilings; per-project quotas are set with the operator
API. Defaults: 64 sessions per IP, 50 HTTP rps per IP, 60 WS connects/min per IP, 20 auth
failures/min per IP, 20 writes/s per API key, 2000 events/s per room, 60 per player.

## Upgrades

Nodes of one deployment should run the same minor version; the room checkpoint format is
versioned in JSON and tolerates added fields.

Rolling upgrade, one node at a time (verified on the reference compose: `stop` returned in
0.4 s, the node logged `shutdown: rooms handed off rooms=1`, the room was listed from the other
node immediately and the first node served `/healthz` 200 again 6 s after `start`):

```sh
docker compose pull            # or build the new image
docker compose up -d --no-deps node-a   # SIGTERM → drain → rooms orphaned in Redis → exit 0
curl -fs http://node-a:8090/healthz     # wait for 200
docker compose up -d --no-deps node-b
```

Players of the drained node get `left{reason:"rehost"}` and rejoin by room name on any node;
the room's state comes from the checkpoint. Keep ≥ 2 nodes per region or a deploy ends matches.

## Migrations

The node applies `migrations/` (sqlx, forward-only, additive) on every start under Postgres'
migration lock, so two nodes starting together do not race. For a controlled rollout run the
new image once with `--migrate-only` (connects, migrates, exits 0 — no Redis needed) before
replacing nodes; the old version keeps running against the migrated schema until it is swapped.
There is no down migration: restore from backup to roll a schema change back.

## Secrets and rotation

| Secret | Where | Rotation |
|---|---|---|
| `auth.grant_secret` | all nodes of a deployment + your backend never sees it (it calls `/v1/grants`) | set the old value as `auth.grant_secret_previous`, new value as `grant_secret`, rolling-restart; grants signed with either verify until you drop `_previous` after ≥ `grant_ttl_secs` |
| `auth.admin_token` | nodes, operators | change and rolling-restart; operator sessions re-authenticate |
| API keys | your backend | `POST /v1/operator/projects/:id/keys` with `expires_in_secs`, switch the backend, revoke the old key — no restart |
| `server.metrics_token` | nodes, Prometheus | change both, rolling-restart |
| Postgres / Redis passwords | nodes, `.env` | change on the server, update `NET__DATABASE__URL` / `NET__REDIS__URL`, rolling-restart |
| Webhook secrets | per webhook | `POST /v1/webhooks/:id/rotate {grace_secs}` — new secret shown once, deliveries carry `X-Aurix-Signature-Previous` until the grace ends (default 24 h) |

Transport tickets (QUIC/WebTransport) are also keyed from `grant_secret` but are issued and
verified by the same node within seconds, so rotation needs no grace period for them.

## Backups

Postgres holds everything durable (projects, keys, room history, usage, audit, modules);
Redis holds live state (room index, checkpoints of orphaned rooms, rate-limit windows) and is
`appendonly yes` in the reference compose, so a Redis restart alone loses nothing.
`scripts/backup.sh [out-dir]` takes a consistent `pg_dump` (single transaction, plain SQL,
gzip) plus a point-in-time Redis RDB (`BGSAVE`, waits for `LASTSAVE` to move) and writes a
manifest with sizes and SHA-256; restore steps are in the script header. Run it from cron on
the compose host (`COMPOSE="docker compose -f deploy/docker-compose.prod.yml --env-file .env"`)
and ship the directory off-host; the dump contains API-key hashes and webhook secrets, so
treat it like the database itself.

## Docker

`Dockerfile` builds a release node image: Debian slim, runs as uid 10001 (`aurix`), `STOPSIGNAL SIGTERM`
(the node drains and hands rooms off; give it `stop_grace_period: 30s`), container healthcheck on
`/healthz` (503 while draining, so orchestrators stop routing to it). Ports: 8090/tcp (HTTP/WS),
7000/udp (QUIC), 7443/udp (WebTransport). Configure with `NET__SECTION__KEY` env vars
(`NET__AUTH__GRANT_SECRET`, `NET__AUTH__ADMIN_TOKEN`, `NET__DATABASE__URL`, `NET__REDIS__URL`,
`NET__SERVER__PUBLIC_HOST`, ...) or mount a TOML and pass `--config /etc/aurix-net/net.toml` (needed
for list values such as `trusted_proxies` / `allowed_origins`). `deploy/docker-compose.prod.yml` is a
two-node reference layout. The image sets `NET__SERVER__HTTP_ADDR=0.0.0.0:8090` (the bare binary
defaults to 127.0.0.1 for dev runs; inside a container that leaves the published port dead while the
in-container healthcheck still passes — found when bringing the reference compose up). The console's
`NET_API_URL` should point at your load balancer / VIP, not one node, or the console returns 502
while that node is being upgraded.

Postgres over TLS: `database.url = "postgres://user:pw@host/aurix_net?sslmode=verify-full&sslrootcert=/etc/aurix-net/pg-ca.pem"`
(`require` for a public CA; the node fails at start on a certificate it cannot verify, it never falls back
to plaintext).

Redis over TLS: `redis.url = "rediss://:password@host:6390/"`; for a private CA set `redis.ca_cert` to the
PEM path (`NET__REDIS__CA_CERT=/etc/aurix-net/redis-ca.pem`), otherwise WebPKI roots are used.

## Monitoring

`/metrics` is Prometheus text (`server.metrics_token` as bearer; keep the port off the public
interface). `deploy/monitoring/` ships a ready stack:

- `prometheus.yml` — scrape config with the token from a file and a `node` label per target;
- `alerts.yml` — rules: node down, sync tick p99 > 10 ms, tick overruns, lockstep desync, > 2 % lost
  sync deltas, credential guessing being rate limited, Redis checkpoint write errors, rooms near
  `max_rooms_per_node`;
- `grafana/` — provisioned datasource and the **Aurix Net — fleet** dashboard (rooms/players per
  node, datagrams and bytes, tick p50/p99, overruns, events by channel, errors by code, sync health,
  desyncs/adopted rooms).

`docker compose -f deploy/docker-compose.prod.yml --profile monitoring up -d` starts Prometheus and
Grafana (`:3000`, password `GRAFANA_PASSWORD`) next to the nodes. For operator-facing alerts (node
stale, project at a usage cap, failing webhooks) the node itself can POST to a webhook — see
[webhooks.md](webhooks.md#operator-alerts-fleet-level); the Prometheus rules cover what only the
process knows (tick time, lost deltas, Redis writes).

## Console

`dashboard/` builds to a Caddy image (`aurix-net-console`, non-root, ~120 MB) that serves the
static console and reverse-proxies `/v1/*` and `/healthz` to one node (`NET_API_URL`, default
`http://net:8090`). It stores nothing: operator tokens and project API keys live in the
operator's browser (local storage) and are revalidated against `/v1/me` on every load.

- Put the console behind the same TLS terminator as the nodes; never expose it on plain HTTP
  outside a private network — the bearer token travels in every request.
- `server.allowed_origins` on the nodes does not need the console origin: the proxy keeps
  everything same-origin.
- The console only needs the node's HTTP port; QUIC/WebTransport ports are for game clients.
- Reference service: `console` in `deploy/docker-compose.prod.yml`. Local dev:
  `cd dashboard && npm ci && NET_API_URL=http://127.0.0.1:8090 npm run dev`.
- Cross-node room detail: a node that does not host the room asks the hosting node over
  `GET /v1/internal/rooms/:id/players` with the fleet-wide `auth.admin_token`, so all nodes must
  share the same admin token and reach each other's `http_url`.
