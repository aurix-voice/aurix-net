# Webhooks

Aurix Net POSTs JSON events about rooms and players to the HTTPS endpoints registered for a project (`POST /v1/webhooks` with `admin` scope, or from the console). Every node has one delivery worker; events are queued off the hot path so a slow receiver never slows a match.

## Request

```http
POST /your/endpoint HTTP/1.1
Content-Type: application/json
X-Aurix-Event: player.joined
X-Aurix-Signature: sha256=<hex HMAC-SHA256 of the raw body with the webhook secret>
X-Aurix-Signature-Previous: sha256=<hex>   # only during a secret-rotation grace period

{"event":"player.joined","project":"<project-uuid>","at":"2026-11-07T16:20:00Z","data":{"room":"<uuid>","actor":3}}
```

The envelope is always `event`, `project`, `at` (RFC 3339, UTC) and `data`. Verify the signature over the **raw** body bytes before parsing.

## Events

| Event | `data` | When |
|---|---|---|
| `room.created` | `{"room":"<uuid>","name":"lobby-1","mode":"relay|sync|lockstep","region":"eu"}` | a room was opened (by a client join with `create`, or by `POST /v1/rooms`) |
| `room.closed` | `{"room":"<uuid>"}` | the room ended: last player left and `empty_ttl` passed, or it was closed by the game / an operator |
| `room.rehosted` | `{"room":"<uuid>","name":"lobby-1","mode":"sync","node":"<node-uuid>","restored":true}` | the room was adopted by another node after a drain or node loss; `restored` is false when no checkpoint was found and the room started fresh |
| `player.joined` | `{"room":"<uuid>","actor":3}` | a player finished auth + join; `actor` is the per-room id (0..N) |
| `player.left` | `{"room":"<uuid>","actor":3,"kicked":true}` | a player left or timed out (`player_ttl`); `kicked` is present only for kicks |
| `match.found` | `{"room":"lobby-7f3a","mode":"sync","players":["p1","p2"],"tickets":["<uuid>","<uuid>"]}` | matchmaking grouped tickets; `room` is the room name the players will join with their grants (`tickets` lists the matched ticket ids) |
| `test` | `{"webhook":"<uuid>"}` | sent by `POST /v1/webhooks/:id/test` or the console; ignores the event filter |

A webhook with an empty `events` list receives every event. Unknown names are rejected at registration.

## Delivery semantics

- Up to 3 attempts per event with 0.5 s / 1 s / 2 s backoff; a 4xx answer stops retries (the receiver rejected it on purpose), 5xx and network errors retry. Request timeout 5 s.
- At-least-once: a receiver that answered slowly may see the same event twice — de-duplicate on (`event`, `data.room`, `data.actor`, `at`) if that matters.
- Ordering is per node, not global: `player.left` for a rehosted room can arrive from a different node than its `player.joined`.
- Outcomes are kept `retention.webhook_deliveries_days` (7) and visible under *Deliveries* in the console or `GET /v1/webhooks/:id/deliveries`: HTTP status, attempts, latency, error text.
- Receivers must be public HTTPS (plain HTTP only in `development`); loopback, private, link-local and cloud-metadata addresses are refused at registration and re-checked at send time (DNS rebinding). Redirects are not followed.

## Secret rotation

`POST /v1/webhooks/:id/rotate` (`{"grace_secs": 86400}`) returns a new secret once. Until the grace period ends (max 7 days) every delivery also carries `X-Aurix-Signature-Previous` signed with the old secret, so switch the receiver whenever convenient and accept either header meanwhile.

## Verifying the signature

Node.js:

```js
import { createHmac, timingSafeEqual } from "node:crypto";

export function verify(rawBody, header, secret) {
  const expected = "sha256=" + createHmac("sha256", secret).update(rawBody).digest("hex");
  return header?.length === expected.length && timingSafeEqual(Buffer.from(header), Buffer.from(expected));
}

// express: app.post('/hook', express.raw({ type: 'application/json' }), (req, res) => {
//   if (!verify(req.body, req.get('x-aurix-signature'), SECRET) && !verify(req.body, req.get('x-aurix-signature-previous'), OLD_SECRET)) return res.sendStatus(401);
//   const ev = JSON.parse(req.body); ... res.sendStatus(204); });
```

Python:

```python
import hmac, hashlib

def verify(raw_body: bytes, header: str | None, secret: str) -> bool:
    expected = "sha256=" + hmac.new(secret.encode(), raw_body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(header or "", expected)
```

Answer 2xx quickly (204 is fine) and do the work asynchronously; anything slower than 5 s counts as a failed attempt.

## Operator alerts (fleet level)

Operators can register their own receivers under **Settings → Alert webhooks** (`GET/POST/DELETE /v1/operator/webhooks[/:id]`, `GET …/:id/deliveries`, `POST …/:id/test`). Same envelope and signature; `project` is `null` and the subject lives in `data`.

| Event | `data` | Fires when |
|---|---|---|
| `node.stale` | node row (`id`, `region`, `last_heartbeat`, …) | a node missed heartbeats for 30 s |
| `node.draining` | node row + `rooms` | a node is draining for a rolling restart |
| `project.suspended` | `{id, name, since}` | a project is suspended |
| `project.usage_cap` | `{project_id, project_name, metric, used, cap, hard}` | ≥80 % of a monthly cap (`hard: false`) and again at 100 % (`hard: true`) |
| `project.rejections` | `{project_id, project_name, total, by}` | players were turned away in the last hour |
| `webhook.failing` | `{project_id, project_name, webhook_id, url, failed, total}` | a project webhook failed in the last hour |
| `test` | `{webhook}` | `POST /v1/operator/webhooks/:id/test` |

One node (Redis leader lock) evaluates the conditions every minute; each subject fires at most once per hour while the condition persists, so a stuck node produces one alert an hour, not sixty. Deliveries are logged like project webhooks (`retention.webhook_deliveries_days`).
