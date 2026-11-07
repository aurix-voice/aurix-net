# Room modes

One server, one client, one wire format. A room is created in one of three modes; the mode
decides who runs the game simulation and what travels on the data path.

| | `relay` | `sync` | `lockstep` |
|---|---|---|---|
| Simulation runs on | clients (your code) | server (authoritative) | every client (deterministic) |
| Client sends | events | inputs | inputs |
| Client receives | events | state deltas | everyone's inputs |
| Cheating resistance | none | strong | medium (hash checks) |
| Late join | cached events | snapshot | journal tail / checkpoint |
| Typical use | lobbies, turn-based, co-op, chat-like traffic | shooters, MMO-lite, anything server-authoritative | RTS, fighting, physics sims, anything that must replay bit-exactly |

All modes share: WebSocket control channel, QUIC/WebTransport data path (WebSocket tunnel
when UDP is blocked), three channels (unreliable-sequenced, reliable-ordered,
reliable-unordered), rooms/lobby/properties/master client, grants and quotas.

## relay

Create with `mode: "relay"` (default). Events are opaque bytes with a code, a target
(`all`, `others`, `master`, explicit actors), an optional interest group and a cache flag so
late joiners receive them. Rate and size limits come from the project quota.

## sync

Create with `mode: "sync"` and optional props:

| prop | default | meaning |
|---|---|---|
| `tick_hz` | 60 | simulation rate (8..128) |
| `schema` | built-in arena | IDL text, see below |
| `budget_bytes` | 1100 | max delta payload per client per tick |
| `aoi_cell`, `aoi_radius` | 32, 2 | interest grid cell size (world units) and radius in cells; `aoi_cell: 0` disables AOI |
| `position` | `Transform.0` | `Component.field` holding the vec3 used for AOI |

Clients send `Input` frames (last 3 ticks, redundant) on the unreliable channel; the server
runs the simulation, replicates per-client deltas (change masks per component, baselines per
client, priority by change/distance/staleness) and keeps a hitbox history for lag
compensation. Simulation events reach clients on the reliable channel. Every 5 s the frame is
checkpointed to Redis for re-hosting.

Without a `schema` the room runs the built-in arena logic (one avatar per actor, input =
`i8 move_x, i8 move_z, u8 buttons`). With a `schema` the room is owner-authoritative: each
actor owns one entity and its input is a list of full component writes that the server
applies and replicates. Server-side game logic in a WASM sandbox is the next step.

### Schema IDL

```text
component Transform {
    pos: vec3 @range(-4096, 4096, 0.01)   // quantized, 3 x 20 bits
    rot: quat                             // smallest-three, 32 bits
    speed: f32                            // raw float
}
component Health {
    hp: u8
    team: i16
    name: string(24)
    flags: bits(5)
}
```

Types: `bool`, `u8..u64`, `i8..i64`, `bits(n)`, `f32`, `f32 @range(min,max,res)`,
`vec2`/`vec3 @range(...)`, `quat`, `string(max)`, `bytes(max)`. The blake3 hash of the
canonical schema is returned in `joined.schema_hash`; a client that sends a different
`schema_hash` on join is refused with `schema_mismatch`.

## lockstep

Create with `mode: "lockstep"` and `tick_hz`. The server is the clock: every tick it bundles
the inputs it received for that tick and sends the bundle (plus the two previous ones) to
every actor. Clients simulate with `net-rollback`: predict missing inputs with the actor's
last one, rewind and resimulate when a late input differs, hash the state every N ticks and
report it (`Hash` frame). The server compares hashes per tick; a minority is reported to
everyone as `desync`. The server keeps the canonical input journal, which is also the replay
format (`net-rollback::Replay`): seed + schema hash + inputs per tick reproduces the game.

Use `net-fpmath` (Q32.32 fixed point, table trig, deterministic RNG) for anything that
touches game state; never floats.

### Late join and rejoin

A lockstep room never stops for a joiner and never migrates a host. Every
`CHECKPOINT_EVERY_SECS` (5 s) the server asks the master for its newest *confirmed* state
(`CheckpointReq` on the reliable channel; the master answers with `Checkpoint`: tick, hash,
<= 60 KiB of state — use `Rollback::confirmed_state()`). When a data link attaches to a room
that is past tick 0 (late join, or a reconnect into an offline slot) the server sends, on the
reliable-ordered channel, `Catchup` (the newest checkpoint, or tick 0 with an empty state
meaning "start from the seed") followed by every journal bundle from that tick up to the
current one, in ~32 KiB frames. Live bundles keep flowing unreliably in parallel, so the
client buffers them until the reliable tail reaches a tick the live stream already covers
(the tail always overlaps thanks to bundle redundancy). `joined.tick` and `joined.seed` tell
the client where it enters. Start the client with `Rollback::from_checkpoint(cfg, actors,
tick, state)` and feed the tail bundles as confirmed inputs. If a joiner had to replay from a
stale checkpoint the server asks the master for a fresh one at once, so catch-up stays bounded.

## Data frames (first byte)

| kind | direction | mode |
|---|---|---|
| event frame (no kind byte, see `net-rooms::EventFrame`) | both | relay, sync events |
| `0x10` Input | client -> server | sync, lockstep |
| `0x11` Delta | server -> client | sync |
| `0x12` Resync | client -> server | sync |
| `0x20` InputBundle | server -> client | lockstep |
| `0x21` Hash | client -> server | lockstep |
| `0x22` Checkpoint | master -> server (reliable) | lockstep |
| `0x23` CheckpointReq | server -> master (reliable) | lockstep |
| `0x24` Catchup | server -> joiner (reliable), then `0x20` bundles | lockstep |
