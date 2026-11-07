import { Link, useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft, Plus } from "lucide-react";
import { useState } from "react";

import type { ApiKey, LiveRoom, Module, Webhook } from "@/api/client";
import { useCreateKey, useOpDeleteModule, useOpDeleteWebhook, useProject, useRegions, useRevokeProjectKey, useSetLimits, useSetSuspended, useDeleteProject, useRejectionsFor } from "@/api/hooks";
import { BansPanel } from "@/components/BansPanel";
import { ConnectCard } from "@/components/ConnectCard";
import { MatchmakingQueue } from "@/components/MatchmakingQueue";
import { PlayerLookup } from "@/components/PlayerLookup";
import { RotateWebhookDialog } from "@/components/RotateWebhook";
import { RejectionsCard } from "@/components/RejectionsCard";
import { useAuth } from "@/auth/AuthProvider";
import { NewKeyDialog } from "@/components/NewKeyDialog";
import { useI18n } from "@/i18n";
import { fmtBytes, fmtDateTimeShort } from "@/lib/format";
import { modeTone } from "@/lib/rooms";
import { keyColumns } from "@/pages/Keys";
import { RequireOperator } from "@/shell/Guards";
import { Button } from "@/ui/Button";
import { WebhookDeliveriesDialog } from "@/components/WebhookDeliveries";
import { ConfirmDialog } from "@/ui/Dialog";
import { Input } from "@/ui/Input";
import { Tabs, TabPanel } from "@/ui/Menu";
import { Loading, PageHeader, QueryError, Section } from "@/ui/Page";
import { Badge, Callout, Card, Field, IdChip, KV, Mono } from "@/ui/Primitives";
import { DataTable } from "@/ui/Table";
import { useToast } from "@/ui/Toast";

const LIMIT_KEYS = ["max_rooms", "max_players_per_room", "max_events_per_sec_per_room", "max_events_per_sec_per_player", "max_event_bytes"] as const;

function initialDraft(limits: Record<string, unknown>): Record<string, string> {
  const next: Record<string, string> = {};
  for (const k of LIMIT_KEYS) {
    const v = limits[k];
    next[k] = typeof v === "number" ? String(v) : "";
  }
  const regions = limits.allowed_regions;
  next.allowed_regions = Array.isArray(regions) ? regions.map(String).join(", ") : "";
  return next;
}

function LimitsForm({ projectId, limits, effective }: { projectId: string; limits: Record<string, unknown>; effective: Record<string, unknown> }) {
  const { t } = useI18n();
  const toast = useToast();
  const setLimits = useSetLimits(projectId);
  const [draft, setDraft] = useState(() => initialDraft(limits ?? {}));
  async function save() {
    const body: Record<string, unknown> = {};
    for (const k of LIMIT_KEYS) {
      const v = draft[k]?.trim();
      if (v) body[k] = Number(v);
    }
    const regions = draft.allowed_regions
      ?.split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    if (regions?.length) body.allowed_regions = regions;
    await setLimits.mutateAsync(body);
    toast.ok(t("common.save"));
  }
  const fallback = (k: string) => {
    const v = effective[k];
    return typeof v === "number" || typeof v === "string" ? String(v) : "—";
  };
  return (
    <Card className="p-4 space-y-3">
      <p className="text-xs text-fg-muted">{t("projects.limits.desc")}</p>
      {LIMIT_KEYS.map((k) => (
        <Field key={k} label={t(`projects.limit.${k}`)} htmlFor={`lim-${k}`} hint={`${t("common.unlimited").toLowerCase()} → ${fallback(k)}`}>
          <Input id={`lim-${k}`} type="number" min={0} value={draft[k] ?? ""} onChange={(e) => setDraft((cur) => ({ ...cur, [k]: e.target.value }))} />
        </Field>
      ))}
      <Field label={t("projects.limit.allowed_regions")} htmlFor="lim-regions">
        <Input id="lim-regions" value={draft.allowed_regions ?? ""} onChange={(e) => setDraft((cur) => ({ ...cur, allowed_regions: e.target.value }))} placeholder="eu, us" />
      </Field>
      <Button onClick={() => void save()} loading={setLimits.isPending} className="w-full">
        {t("projects.limits.save")}
      </Button>
    </Card>
  );
}

function ProjectBody() {
  const { projectId } = useParams({ from: "/shell/projects/$projectId" });
  const { t, locale } = useI18n();
  const { selectProject } = useAuth();
  const q = useProject(projectId);
  const toast = useToast();
  const createKey = useCreateKey(projectId);
  const revokeKey = useRevokeProjectKey(projectId);
  const [revoking, setRevoking] = useState<ApiKey | null>(null);
  const regions = useRegions();
  const setSuspended = useSetSuspended(projectId);
  const deleteProject = useDeleteProject();
  const rejections = useRejectionsFor(projectId);
  const [inspecting, setInspecting] = useState<Webhook | null>(null);
  const [rotating, setRotating] = useState<Webhook | null>(null);
  const navigate = useNavigate();
  const [deletingProject, setDeletingProject] = useState(false);
  const [confirmName, setConfirmName] = useState("");
  const [suspending, setSuspending] = useState(false);
  const delWebhook = useOpDeleteWebhook(projectId);
  const delModule = useOpDeleteModule(projectId);
  const [deleting, setDeleting] = useState<{ kind: "webhook" | "module"; id: string; label: string } | null>(null);
  const [tab, setTab] = useState("keys");
  const [newKey, setNewKey] = useState(false);
  if (q.isLoading) return <Loading />;
  if (q.isError) return <QueryError error={q.error} onRetry={() => void q.refetch()} />;
  const d = q.data!;
  const p = d.project;

  return (
    <>
      <PageHeader
        title={
          <span className="flex items-center gap-3">
            <Link to="/projects" className="text-fg-muted hover:text-fg">
              <ArrowLeft className="size-4" />
            </Link>
            {p.name}
            <Badge>{p.plan}</Badge>
            {p.disabled_at ? <Badge tone="warn">{t("projects.suspended")}</Badge> : null}
          </span>
        }
        description={<IdChip id={p.id} short={36} />}
        actions={
          <div className="flex items-center gap-2">
            <Button variant="ghost" size="sm" className="text-danger" onClick={() => setDeletingProject(true)}>
              {t("projects.delete")}
            </Button>
            <Button variant={p.disabled_at ? "secondary" : "ghost"} size="sm" onClick={() => setSuspending(true)}>
              {p.disabled_at ? t("projects.resume") : t("projects.suspend")}
            </Button>
            <Link to="/rooms" onClick={() => selectProject(p.id)}>
              <Button variant="secondary" size="sm">
                {t("projects.open")} →
              </Button>
            </Link>
          </div>
        }
      />
      {p.disabled_at ? (
        <Callout tone="warn" className="mb-4">
          {t("projects.suspended.callout", { since: fmtDateTimeShort(locale, p.disabled_at) })}
          {d.rooms.length ? ` ${t("projects.suspended.live", { n: d.rooms.length })}` : ""}
        </Callout>
      ) : null}
      <WebhookDeliveriesDialog webhook={inspecting} projectId={p.id} onClose={() => setInspecting(null)} />
      <RotateWebhookDialog webhook={rotating} onClose={() => setRotating(null)} projectId={p.id} />
      <ConfirmDialog
        open={deletingProject}
        onOpenChange={(o) => {
          setDeletingProject(o);
          if (!o) setConfirmName("");
        }}
        title={t("projects.delete")}
        description={t("projects.delete.confirm", { name: p.name })}
        confirmLabel={t("projects.delete")}
        variant="danger"
        disabled={confirmName.trim() !== p.name}
        onConfirm={async () => {
          const r = await deleteProject.mutateAsync(p.id);
          toast.ok(t("projects.delete.done", { rooms: r.closed_rooms, keys: r.revoked_keys }));
          selectProject(null);
          void navigate({ to: "/projects" });
        }}
      >
        <Input
          aria-label={t("common.name")}
          placeholder={p.name}
          value={confirmName}
          onChange={(e) => setConfirmName(e.target.value)}
          autoComplete="off"
        />
      </ConfirmDialog>
      <ConfirmDialog
        open={suspending}
        onOpenChange={setSuspending}
        title={p.disabled_at ? t("projects.resume") : t("projects.suspend")}
        description={p.disabled_at ? t("projects.resume.confirm", { name: p.name }) : t("projects.suspend.confirm", { name: p.name })}
        confirmLabel={p.disabled_at ? t("projects.resume") : t("projects.suspend")}
        variant={p.disabled_at ? "primary" : "danger"}
        onConfirm={async () => {
          const r = await setSuspended.mutateAsync(!p.disabled_at);
          toast.ok(p.disabled_at ? t("projects.resume") : t("projects.suspend"), r.live_rooms ? t("projects.suspended.live", { n: r.live_rooms }) : undefined);
        }}
      />
      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_360px]">
        <Card>
          <Tabs
            value={tab}
            onValueChange={setTab}
            tabs={[
              { value: "keys", label: t("projects.detail.keys"), count: d.keys.length },
              { value: "rooms", label: t("projects.detail.rooms"), count: d.rooms.length },
              { value: "webhooks", label: t("projects.detail.webhooks"), count: d.webhooks.length },
              { value: "modules", label: t("projects.detail.modules"), count: d.modules.length },
              { value: "players", label: t("players.title") },
              { value: "bans", label: t("bans.title") },
              { value: "matchmaking", label: t("nav.matchmaking") },
            ]}
          >
            <TabPanel value="keys">
              <div className="flex justify-end px-3 py-2 border-b border-border">
                <Button size="sm" onClick={() => setNewKey(true)}>
                  <Plus className="size-3.5" /> {t("projects.issueKey")}
                </Button>
              </div>
              <DataTable rows={d.keys} columns={keyColumns(t, locale, setRevoking)} rowKey={(k) => k.id} dense />
            </TabPanel>
            <TabPanel value="rooms">
              <DataTable
                rows={d.rooms}
                rowKey={(r: LiveRoom) => r.room}
                dense
                columns={[
                  { key: "name", header: t("common.name"), cell: (r) => <Link to="/rooms/$roomId" params={{ roomId: r.room }} className="hover:underline">{r.name || r.room}</Link> },
                  { key: "mode", header: t("rooms.mode"), cell: (r) => <Badge tone={modeTone(r.mode)}>{r.mode}</Badge> },
                  { key: "players", header: t("rooms.players"), align: "right", cell: (r) => `${r.players} / ${r.max_players}` },
                  { key: "node", header: t("rooms.node"), cell: (r) => <IdChip id={r.node} /> },
                ]}
              />
            </TabPanel>
            <TabPanel value="webhooks">
              <DataTable
                rows={d.webhooks}
                rowKey={(w: Webhook) => w.id}
                dense
                columns={[
                  { key: "url", header: t("webhooks.url"), cell: (w) => <Mono>{w.url}</Mono> },
                  { key: "events", header: t("webhooks.events"), cell: (w) => (w.events.length ? w.events.join(", ") : "*") },
                  {
                    key: "actions",
                    header: "",
                    align: "right",
                    cell: (w) => (
                      <span className="flex justify-end gap-1">
                        <Button variant="ghost" size="sm" onClick={() => setInspecting(w)}>
                          {t("webhooks.deliveries")}
                        </Button>
                        <Button variant="ghost" size="sm" onClick={() => setRotating(w)}>
                          {t("webhooks.rotate")}
                        </Button>
                        <Button variant="ghost" size="sm" onClick={() => setDeleting({ kind: "webhook", id: w.id, label: w.url })}>
                          {t("common.delete")}
                        </Button>
                      </span>
                    ),
                  },
                ]}
              />
            </TabPanel>
            <TabPanel value="players">
              <PlayerLookup projectId={p.id} />
            </TabPanel>
            <TabPanel value="matchmaking">
              <MatchmakingQueue projectId={p.id} />
            </TabPanel>
            <TabPanel value="bans">
              <BansPanel projectId={p.id} />
            </TabPanel>
            <TabPanel value="modules">
              <DataTable
                rows={d.modules}
                rowKey={(m: Module) => m.name}
                dense
                columns={[
                  { key: "name", header: t("common.name"), cell: (m) => m.name },
                  { key: "hash", header: t("modules.hash"), cell: (m) => <Mono>{m.hash.slice(0, 16)}…</Mono> },
                  { key: "size", header: t("modules.size"), align: "right", cell: (m) => fmtBytes(locale, m.size) },
                  { key: "updated", header: t("modules.updated"), cell: (m) => fmtDateTimeShort(locale, m.updated_at) },
                  {
                    key: "actions",
                    header: "",
                    align: "right",
                    cell: (m) => (
                      <Button variant="ghost" size="sm" onClick={() => setDeleting({ kind: "module", id: m.name, label: m.name })}>
                        {t("common.delete")}
                      </Button>
                    ),
                  },
                ]}
              />
            </TabPanel>
          </Tabs>
        </Card>
        <div className="flex flex-col gap-4">
          <Section title={t("projects.limits")}>
            <LimitsForm key={JSON.stringify(p.limits)} projectId={p.id} limits={p.limits} effective={d.effective_limits} />
          </Section>
          <Card className="p-4">
            <KV cols={1} items={[{ k: t("common.created"), v: fmtDateTimeShort(locale, p.created_at) }, { k: t("projects.plan"), v: p.plan }]} />
          </Card>
          <Section title={t("projects.health")}>
            <RejectionsCard data={rejections.data} loading={rejections.isLoading} />
          </Section>
          <Section title={t("projects.connect")}>
            <ConnectCard regions={regions.data ?? []} allowed={Array.isArray(p.limits?.allowed_regions) ? (p.limits.allowed_regions as string[]) : undefined} />
          </Section>
        </div>
      </div>
      <NewKeyDialog open={newKey} onOpenChange={setNewKey} create={(b) => createKey.mutateAsync(b)} />
      <ConfirmDialog
        open={!!deleting}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={deleting?.kind === "webhook" ? t("webhooks.delete") : t("modules.delete")}
        description={deleting ? t("common.delete.confirm", { name: deleting.label }) : ""}
        confirmLabel={t("common.delete")}
        variant="danger"
        onConfirm={async () => {
          if (!deleting) return;
          if (deleting.kind === "webhook") await delWebhook.mutateAsync(deleting.id);
          else await delModule.mutateAsync(deleting.id);
          toast.ok(t("common.deleted"));
          setDeleting(null);
        }}
      />
      <ConfirmDialog
        open={!!revoking}
        onOpenChange={(o) => !o && setRevoking(null)}
        title={t("keys.revoke")}
        description={revoking ? t("keys.revoke.confirm", { name: revoking.name }) : ""}
        confirmLabel={t("keys.revoke")}
        variant="danger"
        onConfirm={async () => {
          if (revoking) await revokeKey.mutateAsync(revoking.id);
          setRevoking(null);
        }}
      />
    </>
  );
}

export function ProjectDetailPage() {
  return (
    <RequireOperator>
      <ProjectBody />
    </RequireOperator>
  );
}
