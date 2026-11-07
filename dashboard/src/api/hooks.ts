import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo } from "react";

import { useAuth } from "@/auth/AuthProvider";

import { NetClient, type ApiKey, type AuditRow, type Health, type LiveRoom, type Module, type NewKey, type NodeRow, type Project, type PublicNode, type ProjectDetail, type Region, type RoomDetail, type Ticket, type Usage, type Attention, type Ban, type MatchmakingStats, type PlayerLookup, type RotatedWebhook, type Rejections, type Webhook, type WebhookDelivery } from "./client";

export function useApi(): NetClient {
  const { token } = useAuth();
  return useMemo(() => new NetClient(token), [token]);
}

export const qk = {
  health: ["health"] as const,
  regions: ["regions"] as const,
  nodes: ["nodes"] as const,
  projects: ["projects"] as const,
  project: (id: string) => ["projects", id] as const,
  projectUsage: (id: string, from: string, to: string) => ["projects", id, "usage", from, to] as const,
  projectRooms: (id: string) => ["projects", id, "rooms"] as const,
  fleetRooms: ["fleet", "rooms"] as const,
  rooms: ["rooms"] as const,
  room: (id: string) => ["rooms", id] as const,
  usage: (from: string, to: string) => ["usage", from, to] as const,
  fleetUsage: (from: string, to: string) => ["fleet-usage", from, to] as const,
  keys: ["keys"] as const,
  webhooks: ["webhooks"] as const,
  modules: ["modules"] as const,
  audit: (project: string | null) => ["audit", project] as const,
  ticket: (id: string) => ["ticket", id] as const,
};

const LIVE = 5_000;

export function useHealth() {
  const api = useApi();
  return useQuery({ queryKey: qk.health, queryFn: () => api.get<Health>("/healthz"), refetchInterval: LIVE, retry: false });
}
export function useRegions() {
  const api = useApi();
  return useQuery({ queryKey: qk.regions, queryFn: () => api.get<Region[]>("/v1/regions"), refetchInterval: 15_000 });
}
/** Operator: every node row from the DB (including draining/stale). Project key: live nodes only. */
export function useNodes() {
  const api = useApi();
  const { operator } = useAuth();
  return useQuery({
    queryKey: [...qk.nodes, operator],
    queryFn: async () => {
      if (operator) return (await api.get<{ nodes: NodeRow[] }>("/v1/operator/nodes")).nodes;
      const pub = await api.get<PublicNode[]>("/v1/nodes");
      return pub.map(
        (n): NodeRow => ({
          id: n.id,
          region: n.region,
          public_host: n.host,
          quic_port: n.quic_port,
          http_url: n.http_url,
          cert_sha256: n.cert_sha256,
          version: n.version ?? "",
          rooms: n.rooms ?? 0,
          players: n.players ?? 0,
          load_factor: n.load_factor,
          draining: false,
          started_at: n.started_at ?? "",
          last_heartbeat: n.last_heartbeat ?? "",
        }),
      );
    },
    refetchInterval: LIVE,
  });
}
export function useProjects() {
  const api = useApi();
  const { operator } = useAuth();
  return useQuery({ queryKey: qk.projects, queryFn: async () => (await api.get<{ projects: Project[] }>("/v1/operator/projects")).projects, enabled: operator });
}
export function useProject(id: string) {
  const api = useApi();
  return useQuery({ queryKey: qk.project(id), queryFn: () => api.get<ProjectDetail>(`/v1/operator/projects/${id}`), refetchInterval: 10_000 });
}
/** Live rooms in scope: the project's rooms (key or operator-selected project) or the whole fleet. */
export function useRooms() {
  const api = useApi();
  const { operator, projectId } = useAuth();
  return useQuery({
    queryKey: operator ? (projectId ? qk.projectRooms(projectId) : qk.fleetRooms) : qk.rooms,
    queryFn: async () => {
      const path = operator ? (projectId ? `/v1/operator/projects/${projectId}/rooms` : "/v1/operator/rooms") : "/v1/rooms";
      return (await api.get<{ rooms: LiveRoom[] }>(path)).rooms;
    },
    refetchInterval: LIVE,
  });
}
export function useRoom(id: string) {
  const api = useApi();
  const { operator } = useAuth();
  const path = operator ? `/v1/operator/rooms/${id}` : `/v1/rooms/${id}`;
  return useQuery({ queryKey: qk.room(id), queryFn: () => api.get<RoomDetail>(path), refetchInterval: LIVE });
}
export function useKickPlayer(roomId: string) {
  const api = useApi();
  const qc = useQueryClient();
  const { operator } = useAuth();
  return useMutation({
    mutationFn: ({ actor, reason }: { actor: number; reason?: string }) =>
      api.request<void>(operator ? `/v1/operator/rooms/${roomId}/players/${actor}` : `/v1/rooms/${roomId}/players/${actor}`, { method: "DELETE", query: { reason } }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: qk.room(roomId) }),
  });
}
export function useCloseRoom() {
  const api = useApi();
  const qc = useQueryClient();
  const { operator } = useAuth();
  return useMutation({
    mutationFn: (id: string) => api.del(operator ? `/v1/operator/rooms/${id}` : `/v1/rooms/${id}`),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["rooms"] }),
  });
}
export function useUsage(from: string, to: string) {
  const api = useApi();
  const { operator, projectId } = useAuth();
  const path = operator ? (projectId ? `/v1/operator/projects/${projectId}/usage` : "/v1/operator/usage") : "/v1/usage";
  const key = operator ? (projectId ? qk.projectUsage(projectId, from, to) : qk.fleetUsage(from, to)) : qk.usage(from, to);
  return useQuery({ queryKey: key, queryFn: () => api.get<Usage>(path, { from, to }) });
}
export function useKeys() {
  const api = useApi();
  return useQuery({ queryKey: qk.keys, queryFn: async () => (await api.get<{ keys: ApiKey[] }>("/v1/keys")).keys });
}
export function useCreateKey(projectId?: string) {
  const api = useApi();
  const qc = useQueryClient();
  const { operator } = useAuth();
  return useMutation({
    mutationFn: (body: { name: string; scopes: string[]; expires_in_secs?: number }) => api.post<NewKey>(operator && projectId ? `/v1/operator/projects/${projectId}/keys` : "/v1/keys", body),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: qk.keys });
      if (projectId) void qc.invalidateQueries({ queryKey: qk.project(projectId) });
    },
  });
}
export function useRevokeProjectKey(projectId: string) {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.del(`/v1/operator/projects/${projectId}/keys/${id}`),
    onSuccess: () => void qc.invalidateQueries({ queryKey: qk.project(projectId) }),
  });
}
export function useRevokeKey() {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({ mutationFn: (id: string) => api.del(`/v1/keys/${id}`), onSuccess: () => void qc.invalidateQueries({ queryKey: qk.keys }) });
}
export function useWebhooks() {
  const api = useApi();
  return useQuery({ queryKey: qk.webhooks, queryFn: async () => (await api.get<{ webhooks: Webhook[] }>("/v1/webhooks")).webhooks });
}
export function useCreateWebhook() {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({ mutationFn: (body: { url: string; events: string[] }) => api.post<Webhook>("/v1/webhooks", body), onSuccess: () => void qc.invalidateQueries({ queryKey: qk.webhooks }) });
}
/** Delivery log of one webhook. `projectId` switches to the operator route. */
/** Why the project's players/keys were turned away in the last N hours (fleet-wide, from Redis). */
export function useRejections(hours = 24) {
  const api = useApi();
  const { operator, projectId } = useAuth();
  return useQuery({
    queryKey: ["rejections", operator ? projectId : "me", hours],
    enabled: !operator || !!projectId,
    refetchInterval: 15_000,
    queryFn: () => api.get<Rejections>(operator ? `/v1/operator/projects/${projectId}/rejections` : "/v1/rejections", { hours }),
  });
}
/** Player deny-list. `projectId` switches to the operator routes. */
/** Open matchmaking tickets + matches made. `projectId` switches to the operator route. */
export function useMatchmakingStats(projectId?: string) {
  const api = useApi();
  return useQuery({
    queryKey: ["mm-stats", projectId ?? "me"],
    refetchInterval: 3000,
    queryFn: () => api.get<MatchmakingStats>(projectId ? `/v1/operator/projects/${projectId}/matchmaking` : "/v1/matchmaking"),
  });
}
/** Fleet-wide "where is this player": live rooms on every node, ban state, audit trail. */
export function usePlayerLookup(playerId: string, projectId?: string) {
  const api = useApi();
  const id = playerId.trim();
  return useQuery({
    queryKey: ["player", projectId ?? "me", id],
    enabled: id.length > 0,
    refetchInterval: 5000,
    queryFn: () => api.get<PlayerLookup>(projectId ? `/v1/operator/projects/${projectId}/players/${encodeURIComponent(id)}` : `/v1/players/${encodeURIComponent(id)}`),
  });
}
export function useAttention() {
  const api = useApi();
  const { operator } = useAuth();
  return useQuery({ queryKey: ["attention"], enabled: operator, refetchInterval: 15_000, queryFn: () => api.get<Attention>("/v1/operator/attention") });
}
export function useBans(projectId?: string) {
  const api = useApi();
  return useQuery({
    queryKey: ["bans", projectId ?? "me"],
    queryFn: async () => (await api.get<{ bans: Ban[] }>(projectId ? `/v1/operator/projects/${projectId}/bans` : "/v1/bans")).bans,
  });
}
export function useAddBan(projectId?: string) {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: { player_id: string; reason?: string; duration_secs?: number }) =>
      api.post<{ ban: Ban }>(projectId ? `/v1/operator/projects/${projectId}/bans` : "/v1/bans", body),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["bans", projectId ?? "me"] }),
  });
}
export function useRemoveBan(projectId?: string) {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (playerId: string) =>
      api.del(projectId ? `/v1/operator/projects/${projectId}/bans/${encodeURIComponent(playerId)}` : `/v1/bans/${encodeURIComponent(playerId)}`),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["bans", projectId ?? "me"] }),
  });
}
export function useRejectionsFor(projectId: string, hours = 24) {
  const api = useApi();
  return useQuery({
    queryKey: ["rejections", projectId, hours],
    refetchInterval: 15_000,
    queryFn: () => api.get<Rejections>(`/v1/operator/projects/${projectId}/rejections`, { hours }),
  });
}
export function useWebhookDeliveries(webhookId: string | null, projectId?: string) {
  const api = useApi();
  return useQuery({
    queryKey: ["webhook-deliveries", projectId ?? "me", webhookId],
    enabled: !!webhookId,
    refetchInterval: 3000,
    queryFn: async () =>
      (
        await api.get<{ deliveries: WebhookDelivery[] }>(
          projectId ? `/v1/operator/projects/${projectId}/webhooks/${webhookId}/deliveries` : `/v1/webhooks/${webhookId}/deliveries`,
          { limit: 50 },
        )
      ).deliveries,
  });
}
export function useTestWebhook(projectId?: string) {
  const api = useApi();
  return useMutation({
    mutationFn: (webhookId: string) =>
      api.post<{ queued: boolean }>(projectId ? `/v1/operator/projects/${projectId}/webhooks/${webhookId}/test` : `/v1/webhooks/${webhookId}/test`),
  });
}
/** New signing secret (shown once); the previous one keeps signing for the grace period. */
export function useRotateWebhook(projectId?: string) {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ webhookId, graceSecs }: { webhookId: string; graceSecs?: number }) =>
      api.post<RotatedWebhook>(projectId ? `/v1/operator/projects/${projectId}/webhooks/${webhookId}/rotate` : `/v1/webhooks/${webhookId}/rotate`, { grace_secs: graceSecs }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: qk.webhooks });
      if (projectId) void qc.invalidateQueries({ queryKey: qk.project(projectId) });
    },
  });
}
export function useDeleteWebhook() {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({ mutationFn: (id: string) => api.del(`/v1/webhooks/${id}`), onSuccess: () => void qc.invalidateQueries({ queryKey: qk.webhooks }) });
}
export function useModules() {
  const api = useApi();
  return useQuery({ queryKey: qk.modules, queryFn: async () => (await api.get<{ modules: Module[] }>("/v1/modules")).modules });
}
export function usePutModule() {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ name, file }: { name: string; file: File }) => api.request<Module>(`/v1/modules/${encodeURIComponent(name)}`, { method: "PUT", raw: file, contentType: "application/wasm", timeoutMs: 60_000 }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: qk.modules }),
  });
}
export function useDeleteModule() {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({ mutationFn: (name: string) => api.del(`/v1/modules/${encodeURIComponent(name)}`), onSuccess: () => void qc.invalidateQueries({ queryKey: qk.modules }) });
}
export interface AuditFilter {
  action?: string;
  actor?: string;
  limit?: number;
}
export function useAudit(f: AuditFilter = {}) {
  const api = useApi();
  const { operator, projectId } = useAuth();
  const limit = f.limit ?? 200;
  return useQuery({
    queryKey: [...qk.audit(operator ? projectId : "me"), limit, f.action ?? "", f.actor ?? ""],
    queryFn: async () =>
      (
        await api.get<{ audit: AuditRow[] }>(operator ? "/v1/operator/audit" : "/v1/audit", {
          limit,
          project: operator ? projectId : undefined,
          action: f.action || undefined,
          actor: f.actor || undefined,
        })
      ).audit,
  });
}
export function useOpDeleteWebhook(projectId: string) {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.del(`/v1/operator/projects/${projectId}/webhooks/${id}`),
    onSuccess: () => void qc.invalidateQueries({ queryKey: qk.project(projectId) }),
  });
}
export function useOpDeleteModule(projectId: string) {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (name: string) => api.del(`/v1/operator/projects/${projectId}/modules/${encodeURIComponent(name)}`),
    onSuccess: () => void qc.invalidateQueries({ queryKey: qk.project(projectId) }),
  });
}
export function useCreateProject() {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: { name: string; plan?: string }) => api.post<{ project: Project; key: NewKey }>("/v1/operator/projects", body),
    onSuccess: () => void qc.invalidateQueries({ queryKey: qk.projects }),
  });
}
export function useSetLimits(projectId: string) {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({ mutationFn: (limits: Record<string, unknown>) => api.post(`/v1/operator/projects/${projectId}/limits`, limits), onSuccess: () => void qc.invalidateQueries({ queryKey: qk.project(projectId) }) });
}
export function useSetSuspended(projectId: string) {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (suspended: boolean) => api.post<{ project: Project; live_rooms: number }>(`/v1/operator/projects/${projectId}/suspend`, { suspended }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: qk.project(projectId) });
      void qc.invalidateQueries({ queryKey: qk.projects });
    },
  });
}
export function useSetNodeDraining() {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ nodeId, draining }: { nodeId: string; draining: boolean }) =>
      api.post<{ node_id: string; draining: boolean; rooms_handed_off: number }>(`/v1/operator/nodes/${nodeId}/drain`, { draining }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: qk.nodes });
      void qc.invalidateQueries({ queryKey: qk.regions });
    },
  });
}
export function useDeleteProject() {
  const api = useApi();
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (projectId: string) => api.del<{ closed_rooms: number; revoked_keys: number }>(`/v1/operator/projects/${projectId}`),
    onSuccess: () => void qc.invalidateQueries({ queryKey: qk.projects }),
  });
}
export function useFileTicket() {
  const api = useApi();
  return useMutation({ mutationFn: (body: Record<string, unknown>) => api.post<Ticket>("/v1/matchmaking/tickets", body) });
}
export function useTicket(id: string | null) {
  const api = useApi();
  return useQuery({ queryKey: qk.ticket(id ?? ""), queryFn: () => api.get<Ticket>(`/v1/matchmaking/tickets/${id}`), enabled: !!id, refetchInterval: 1_500 });
}
export function useCancelTicket() {
  const api = useApi();
  return useMutation({ mutationFn: (id: string) => api.del(`/v1/matchmaking/tickets/${id}`) });
}
