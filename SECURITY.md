# Security

## Reporting

Email security@aurix.dev (PGP key on the website). We acknowledge within 2 business days and
aim to ship fixes for confirmed issues within 14 days. Please do not open public issues for
vulnerabilities.

## Threat model

Aurix Net nodes are reachable from the public internet on three ports (HTTP control plane,
QUIC data, WebTransport data). We assume:

- **Game clients are hostile.** Anything a client can send is validated for size, rate and
  semantics; a client can only act as the player its grant names, inside the room it joined.
- **Tenants (projects) are mutually untrusted.** Projects are isolated by API key / grant; a
  project's WASM logic runs in a sandbox with fuel, memory and module-size caps and cannot
  reach the host, other rooms or the network.
- **The node's own infrastructure (Postgres, Redis, other nodes) is trusted** and must live on
  a private network (see `docs/deployment.md`).

## Controls

| Area | Control |
|---|---|
| API keys | stored as SHA-256 hashes; `an_<prefix>_<secret>` format; scopes (`rooms`, `read`, `admin`); per-key write budget (`limits.writes_per_sec_per_key`) |
| Operator token | constant-time comparison; empty token disables operator endpoints |
| Grants | HS256 JWT signed with `auth.grant_secret` (≥ 32 random bytes enforced outside development); `exp` checked with 5 s leeway; scopes limited to `join/create/master/spectate`; `room` binding; `meta` ≤ 1 KiB; control characters stripped |
| Data tickets | HMAC-SHA256 over session/room/actor, 30 s TTL; constant-time check; a stale data session cannot act on a slot a newer one owns |
| Transport | QUIC/WebTransport over TLS 1.3 with certificate pinning (`cert_sha256`) for self-signed deployments; QUIC Retry (address validation) before any handshake work; per-IP connection cap |
| Control plane | per-IP HTTP budget, per-IP WebSocket connect budget, auth-failure budget per IP (shared by WebSocket grant failures and REST 401s on API keys / operator tokens; spent budget turns 401 into 429, valid callers unaffected), 10 s pre-auth timeout, 8 KiB pre-auth message cap, 90 s idle timeout, 64 KiB body cap (8 MiB for module upload) |
| Rooms | room/player props size caps, per-room and per-player event budgets, input-rate and look-ahead caps, max players per room/node, project quotas |
| Webhooks | https only outside development, no credentials in URL, no redirects, private/loopback/link-local/metadata hosts refused both at registration and at delivery (DNS rebinding) |
| WASM | wasmtime with fuel (50 M per call), 64 MiB memory, 8 MiB module; a trapping module is disabled, the node is unaffected |
| Shutdown | SIGTERM/Ctrl-C drains: fresh checkpoints to Redis, rooms become `orphaned` for another node to adopt, `/healthz` answers 503 |
| Dependencies | `cargo audit` / `cargo deny` clean (see `deny.toml`) |

## Not covered (by design)

- Cheating inside the rules of a game: that is the job of server-authoritative logic (sync
  mode, WASM) or deterministic verification (lockstep with a server-stepped module).
- DDoS at volumetric scale: put nodes behind your provider's L3/L4 protection; the node
  limits above handle application-level floods.
- Grants are bearer tokens: anyone holding a grant is that player until it expires. Issue
  them with short TTLs from your backend only.

### Per-IP limits and shared addresses

Per-IP budgets are DoS backstops, not fairness controls: whole campuses, mobile carriers (CGNAT) and
LAN parties sit behind one address, so the defaults are deliberately loose (512 sessions, 200 HTTP
requests/s, 600 WebSocket connects/min, 20 failed auths/min per IP) and verified with a 128-player
join burst from a single host. Project quotas (`max_rooms`, `max_players_per_room`, per-room/per-player event rates, event bytes)
and per-player input budgets carry the per-tenant fairness; grant minting has its own per-key budget
(`grants_per_sec_per_key`, 500/s) because it is every player login of the tenant's backend; tune `[limits]` down only behind a proxy that passes
`X-Forwarded-For` from `server.trusted_proxies`.
