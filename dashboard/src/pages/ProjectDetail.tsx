import { Link, useParams } from "@tanstack/react-router";
import { ArrowLeft, Plus } from "lucide-react";
import { useState } from "react";

import type { LiveRoom, Module, Webhook } from "@/api/client";
import { useCreateKey, useProject, useSetLimits } from "@/api/hooks";
import { useAuth } from "@/auth/AuthProvider";
import { NewKeyDialog } from "@/components/NewKeyDialog";
import { useI18n } from "@/i18n";
import { fmtBytes, fmtDateTimeShort } from "@/lib/format";
import { modeTone } from "@/lib/rooms";
import { keyColumns } from "@/pages/Keys";
import { RequireOperator } from "@/shell/Guards";
import { Button } from "@/ui/Button";
import { Input } from "@/ui/Input";
import { Tabs, TabPanel } from "@/ui/Menu";
import { Loading, PageHeader, QueryError, Section } from "@/ui/Page";
import { Badge, Card, Field, IdChip, KV, Mono } from "@/ui/Primitives";
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
  const createKey = useCreateKey(projectId);
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
          </span>
        }
        description={<IdChip id={p.id} short={36} />}
        actions={
          <Link to="/rooms" onClick={() => selectProject(p.id)}>
            <Button variant="secondary" size="sm">
              {t("projects.open")} →
            </Button>
          </Link>
        }
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
            ]}
          >
            <TabPanel value="keys">
              <div className="flex justify-end px-3 py-2 border-b border-border">
                <Button size="sm" onClick={() => setNewKey(true)}>
                  <Plus className="size-3.5" /> {t("projects.issueKey")}
                </Button>
              </div>
              <DataTable rows={d.keys} columns={keyColumns(t, locale)} rowKey={(k) => k.id} dense />
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
                ]}
              />
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
        </div>
      </div>
      <NewKeyDialog open={newKey} onOpenChange={setNewKey} create={(b) => createKey.mutateAsync(b)} />
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
