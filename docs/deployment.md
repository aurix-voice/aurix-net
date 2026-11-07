# Deploying a node (production checklist)

## Network

- Postgres and Redis on a private network only, with authentication (`DATABASE_URL`
  with a strong password and `sslmode=require`; Redis `requirepass`/ACL and TLS or a private
  link). The node never exposes them.
- Public ports: `server.http_addr` (TCP, control plane + WS tunnel), `transport.quic_addr`
  (UDP), `transport.webtransport_addr` (UDP). Everything else closed.
- Terminate TLS for the HTTP port at a reverse proxy or load balancer (the node speaks plain
  HTTP there) and put the proxy's address range into `server.trusted_proxies` so per-IP limits
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
failures/min per IP, 20 writes/s per API key, 500 events/s per room, 60 per player.

## Upgrades

Nodes of one deployment should run the same minor version; the room checkpoint format is
versioned in JSON and tolerates added fields.
