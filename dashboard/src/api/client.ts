import { translate, type Locale } from "@/i18n";

/** Same-origin: Caddy (image) or Vite (dev) proxies `/v1` and `/healthz` to one Aurix Net node. */
export function baseUrl(): string {
  return window.location.origin;
}

export class NetError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "NetError";
  }
}

export class NetNetworkError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "NetNetworkError";
  }
}

export type ErrorKind = "network" | "unauthorized" | "forbidden" | "notFound" | "conflict" | "rateLimited" | "validation" | "server" | "unknown";

export function errorKind(err: unknown): ErrorKind {
  if (err instanceof NetNetworkError) return "network";
  if (err instanceof NetError) {
    if (err.status === 401) return "unauthorized";
    if (err.status === 403) return "forbidden";
    if (err.status === 404) return "notFound";
    if (err.status === 409) return "conflict";
    if (err.status === 429) return "rateLimited";
    if (err.status === 400 || err.status === 422) return "validation";
    if (err.status >= 500) return "server";
  }
  return "unknown";
}

export function errorCode(err: unknown): string | null {
  return err instanceof NetError ? err.code : null;
}

export function isStatus(err: unknown, status: number): boolean {
  return err instanceof NetError && err.status === status;
}

export function errorMessage(err: unknown, locale: Locale = "en"): string {
  if (err instanceof NetNetworkError) return translate(locale, "error.network");
  if (err instanceof NetError) {
    const server = err.message.trim();
    if (server && server !== err.code) return server;
    return translate(locale, "error.withCode", { code: err.code || String(err.status) });
  }
  if (err instanceof Error) return err.message;
  return String(err);
}

export interface RequestOptions {
  method?: "GET" | "POST" | "PUT" | "DELETE";
  query?: Record<string, string | number | undefined | null>;
  body?: unknown;
  /** Raw body (module upload). */
  raw?: BodyInit;
  contentType?: string;
  timeoutMs?: number;
}

/** Thin fetch wrapper: bearer token, JSON in/out, typed errors from `{error, message}` bodies. */
export class NetClient {
  constructor(private readonly token: string | null) {}

  async request<T>(path: string, opts: RequestOptions = {}): Promise<T> {
    const url = new URL(path, baseUrl());
    for (const [k, v] of Object.entries(opts.query ?? {})) {
      if (v !== undefined && v !== null && v !== "") url.searchParams.set(k, String(v));
    }
    const headers: Record<string, string> = { accept: "application/json" };
    if (this.token) headers.authorization = `Bearer ${this.token}`;
    let body: BodyInit | undefined;
    if (opts.raw !== undefined) {
      body = opts.raw;
      headers["content-type"] = opts.contentType ?? "application/octet-stream";
    } else if (opts.body !== undefined) {
      body = JSON.stringify(opts.body);
      headers["content-type"] = "application/json";
    }
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), opts.timeoutMs ?? 15_000);
    let res: Response;
    try {
      res = await fetch(url, { method: opts.method ?? "GET", headers, body, signal: ctrl.signal });
    } catch (e) {
      throw new NetNetworkError(e instanceof Error ? e.message : "network error");
    } finally {
      clearTimeout(timer);
    }
    if (res.status === 204) return undefined as T;
    const text = await res.text();
    let json: unknown;
    try {
      json = text ? JSON.parse(text) : null;
    } catch {
      json = null;
    }
    if (!res.ok) {
      const o = (json ?? {}) as { error?: string; message?: string };
      throw new NetError(res.status, o.error ?? `http_${res.status}`, o.message ?? text.slice(0, 200) ?? res.statusText);
    }
    return json as T;
  }

  get<T>(path: string, query?: RequestOptions["query"]) {
    return this.request<T>(path, { query });
  }
  post<T>(path: string, body?: unknown) {
    return this.request<T>(path, { method: "POST", body });
  }
  del<T = void>(path: string) {
    return this.request<T>(path, { method: "DELETE" });
  }
}

// ---- API shapes (mirror crates/net-api/src/rest.rs)

export interface Me {
  kind: "operator" | "project";
  region: string;
  node_id?: string;
  project?: Project;
  key?: ApiKey;
}
export interface Project {
  id: string;
  name: string;
  plan: string;
  limits: Record<string, unknown>;
  created_at: string;
  disabled_at?: string | null;
}
export interface ApiKey {
  id: string;
  prefix: string;
  name: string;
  scopes: string[];
  created_at: string;
  last_used_at: string | null;
  revoked_at: string | null;
  expires_at?: string | null;
}
export interface NewKey extends Pick<ApiKey, "id" | "prefix" | "name" | "scopes" | "expires_at"> {
  key: string;
}
export interface Region {
  id: string;
  display_name: string;
  nodes: number;
  endpoints: string[];
}
export interface NodeRow {
  id: string;
  region: string;
  public_host: string;
  quic_port: number;
  http_url: string;
  cert_sha256: string;
  version: string;
  rooms: number;
  players: number;
  load_factor: number;
  draining: boolean;
  started_at: string;
  last_heartbeat: string;
}
/** `/v1/nodes`: the public, key-scoped projection of a live node. */
export interface PublicNode {
  id: string;
  region: string;
  host: string;
  quic_port: number;
  http_url: string;
  cert_sha256: string;
  load_factor: number;
  version?: string;
  rooms?: number;
  players?: number;
  started_at?: string;
  last_heartbeat?: string;
}
export interface LiveRoom {
  room: string;
  node: string;
  name: string;
  mode: string;
  players: number;
  max_players: number;
  open: boolean;
  visible: boolean;
  props: Record<string, unknown>;
  project_id?: string;
  project_name?: string;
}
export interface RoomRow {
  id: string;
  project_id: string;
  node_id: string | null;
  region: string;
  name: string;
  mode: string;
  state: string;
  max_players: number;
  visible: boolean;
  open: boolean;
  schema_hash: string | null;
  props: Record<string, unknown>;
  lobby_props: string[];
  created_at: string;
  closed_at: string | null;
}
export interface RoomDetail {
  room: RoomRow;
  live: LiveRoom | null;
  players: Array<{ actor: number; player_id: string; name: string; online: boolean }>;
  players_source?: "local" | "remote" | "unavailable" | "none";
}
export interface UsageRow {
  hour: string;
  metric: string;
  quantity: number;
}
export interface Usage {
  from: string;
  to: string;
  usage: UsageRow[];
}
export interface Webhook {
  id: string;
  url: string;
  events: string[];
  created_at?: string;
  /** Only in the create response. */
  secret?: string;
  /** Set after a rotation while the previous secret still signs. */
  previous_secret_expires_at?: string | null;
}
export interface RotatedWebhook {
  id: string;
  secret: string;
  previous_secret_expires_at: string;
}
export interface MatchmakingStats {
  searching: { id: string; player_id: string; mode: string; region: string | null; max_players: number; age_secs: number }[];
  matched: { last_hour: number; last_24h: number };
}
export interface PlayerLookup {
  player_id: string;
  rooms: { room_id: string; name: string; mode: string; node_id: string; actor: number; player_name: string; online: boolean }[];
  ban: Ban | null;
  audit: AuditRow[];
  unreachable_nodes: { node_id: string; http_url: string }[];
}
export interface NodeStats {
  node_id: string;
  version: string;
  region: string;
  uptime_secs: number;
  draining: boolean;
  rooms: number;
  rooms_by_mode: Record<string, number>;
  players: number;
  spectators: number;
  control_sessions: number;
  datagrams_in: number;
  datagrams_out: number;
  bytes_in: number;
  bytes_out: number;
  sync_ticks: number;
  sync_tick_avg_ms: number;
  sync_tick_p99_ms: number;
  tick_overruns: number;
  sync_lost_deltas: number;
  sync_resyncs: number;
  lockstep_desyncs: number;
  rooms_adopted: number;
  rss_mb: number | null;
}
export interface OperatorToken {
  id: string;
  name: string;
  prefix: string;
  created_by: string;
  created_at: string;
  expires_at: string | null;
  revoked_at: string | null;
  last_used_at: string | null;
}
export interface NewOperatorToken extends OperatorToken {
  token: string;
}
export interface Attention {
  stale_nodes: { id: string; public_host: string; quic_port: number; region: string; last_heartbeat: string }[];
  draining_nodes: { id: string; public_host: string; quic_port: number; region: string; rooms: number }[];
  suspended_projects: { id: string; name: string; since: string }[];
  failing_webhooks: { project_id: string; project_name: string; webhook_id: string; url: string; failed: number; total: number }[];
  rejections: { project_id: string; project_name: string; total: number; by: Record<string, number> }[];
  usage_caps: { project_id: string; project_name: string; metric: "player_minutes" | "bytes"; used: number; cap: number }[];
}
export interface Ban {
  project_id: string;
  player_id: string;
  reason: string;
  created_by: string;
  expires_at: string | null;
  created_at: string;
}
export interface Rejections {
  hours: number;
  total: Record<string, number>;
  series: { t: string; counts: Record<string, number> }[];
}
export interface WebhookDelivery {
  id: string;
  webhook_id: string;
  event: string;
  ok: boolean;
  status: number | null;
  attempts: number;
  latency_ms: number;
  error: string | null;
  created_at: string;
}
export interface Module {
  name: string;
  hash: string;
  size: number;
  updated_at: string;
}
export interface AuditRow {
  id: number;
  project_id: string | null;
  actor: string;
  action: string;
  target: string;
  details: Record<string, unknown>;
  created_at: string;
}
export interface ProjectDetail {
  project: Project;
  effective_limits: Record<string, unknown>;
  keys: ApiKey[];
  webhooks: Webhook[];
  modules: Module[];
  rooms: LiveRoom[];
  webhooks_1h: { failed: number; total: number };
  usage_month: { player_minutes: number; bytes: number; since: string };
}
export interface Health {
  ok: boolean;
  draining: boolean;
  version: string;
  node_id: string;
  region: string;
  rooms: number;
  players: number;
  uptime_secs: number;
}
export interface Ticket {
  ticket: string;
  state: string;
  age_secs?: number;
  room?: string;
}
