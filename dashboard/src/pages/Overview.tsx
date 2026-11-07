import { Link } from "@tanstack/react-router";
import { useMemo } from "react";
import { ArrowRight } from "lucide-react";

import type { NodeRow } from "@/api/client";
import { useAttention, useHealth, useNodes, useRooms, useUsage } from "@/api/hooks";
import { AttentionPanel } from "@/components/AttentionPanel";
import { ChartCard, TimeSeriesChart, type SeriesDef } from "@/components/charts";
import { presetRange } from "@/components/RangePicker";
import { toPoints } from "@/pages/Usage";
import { useAuth } from "@/auth/AuthProvider";
import { useI18n } from "@/i18n";
import { fmtDuration, fmtNumber } from "@/lib/format";
import { modeTone, nodeStale } from "@/lib/rooms";
import { Button } from "@/ui/Button";
import { PageHeader, QueryError, Section } from "@/ui/Page";
import { Badge, Card, CardHeader, EmptyState, IdChip, Mono, Skeleton, Stat } from "@/ui/Primitives";
import { DataTable, type Column } from "@/ui/Table";

export function OverviewPage() {
  const { t, locale } = useI18n();
  const { operator, projectId } = useAuth();
  const health = useHealth();
  const nodes = useNodes();
  const rooms = useRooms();
  const h = health.data;
  const attention = useAttention();
  const live = (nodes.data ?? []).filter((n) => !nodeStale(n.last_heartbeat) || !operator);
  const players = (rooms.data ?? []).reduce((a, r) => a + r.players, 0);
  const day = useMemo(() => presetRange("24h"), []);
  const usage = useUsage(day.from, day.to);
  const usagePoints = useMemo(() => toPoints(usage.data), [usage.data]);
  const usageSeries: SeriesDef[] = [
    { key: "player_minutes", label: t("usage.player_minutes"), color: "var(--fg)", area: true },
    { key: "room_minutes", label: t("usage.room_minutes"), color: "var(--accent)" },
  ];

  const nodeCols: Column<NodeRow>[] = [
    { key: "id", header: t("nodes.host"), cell: (n) => <Mono>{n.public_host}</Mono> },
    { key: "region", header: t("nodes.region"), cell: (n) => n.region },
    { key: "rooms", header: t("nodes.rooms"), align: "right", cell: (n) => fmtNumber(locale, n.rooms) },
    { key: "players", header: t("nodes.players"), align: "right", cell: (n) => fmtNumber(locale, n.players) },
    {
      key: "status",
      header: t("nodes.status"),
      cell: (n) => (n.draining ? <Badge tone="warn" dot>{t("nodes.status.draining")}</Badge> : nodeStale(n.last_heartbeat) ? <Badge tone="danger" dot>{t("nodes.status.stale")}</Badge> : <Badge tone="ok" dot>{t("nodes.status.live")}</Badge>),
    },
  ];

  return (
    <>
      <PageHeader title={t("overview.title")} description={t("overview.desc")} />
      {operator ? <div className="mb-5"><AttentionPanel data={attention.data} loading={attention.isLoading} /></div> : null}
      {health.isError ? <QueryError error={health.error} onRetry={() => void health.refetch()} /> : null}
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4 mb-6">
        <Stat label={t("overview.node")} value={h ? <span className="flex items-center gap-2">{h.draining ? <Badge tone="warn" dot>{t("overview.draining")}</Badge> : <Badge tone="ok" dot>{t("overview.ready")}</Badge>}</span> : <Skeleton className="h-6 w-20" />} hint={h ? <IdChip id={h.node_id} /> : null} />
        <Stat label={t("overview.region")} value={h ? <Mono>{h.region}</Mono> : <Skeleton className="h-6 w-16" />} hint={h ? `${t("overview.version")} ${h.version}` : null} />
        <Stat label={operator && !projectId ? t("overview.liveRooms") : t("overview.rooms")} value={rooms.data ? fmtNumber(locale, rooms.data.length) : <Skeleton className="h-6 w-10" />} hint={h ? `${t("overview.node")}: ${fmtNumber(locale, h.rooms)}` : null} />
        <Stat label={t("overview.players")} value={rooms.data ? fmtNumber(locale, players) : <Skeleton className="h-6 w-10" />} hint={h ? `${t("overview.uptime")} ${fmtDuration(locale, h.uptime_secs)}` : null} />
      </div>
      <div className="grid gap-4 xl:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <Section
          title={t("overview.nodes")}
          actions={
            <Link to="/nodes">
              <Button variant="ghost" size="sm">
                {t("nav.nodes")} <ArrowRight className="size-3.5" />
              </Button>
            </Link>
          }
        >
          <Card>
            <DataTable rows={live} columns={nodeCols} rowKey={(n) => n.id} loading={nodes.isLoading} error={nodes.error} dense skeletonRows={3} />
          </Card>
        </Section>
        <Section
          title={t("overview.liveRooms")}
          actions={
            <Link to="/rooms">
              <Button variant="ghost" size="sm">
                {t("nav.rooms")} <ArrowRight className="size-3.5" />
              </Button>
            </Link>
          }
        >
          <Card>
            <CardHeader title={t("rooms.title")} description={`${fmtNumber(locale, rooms.data?.length ?? 0)} · ${fmtNumber(locale, players)} ${t("overview.players").toLowerCase()}`} />
            {rooms.data && rooms.data.length === 0 ? (
              <EmptyState compact title={t("rooms.empty")} description={t("rooms.empty.desc")} />
            ) : (
              <ul className="divide-y divide-border">
                {(rooms.data ?? []).slice(0, 8).map((r) => (
                  <li key={r.room} className="flex items-center gap-3 px-4 py-2 text-[13px]">
                    <Badge tone={modeTone(r.mode)}>{r.mode}</Badge>
                    <Link to="/rooms/$roomId" params={{ roomId: r.room }} className="font-medium truncate hover:underline">
                      {r.name || r.room}
                    </Link>
                    <span className="ml-auto text-fg-muted tabular-nums">
                      {r.players}/{r.max_players}
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </Card>
        </Section>
        <ChartCard title={t("overview.usage24h")} description={t("usage.desc")} series={usageSeries} query={usage}>
          <TimeSeriesChart data={usagePoints} series={usageSeries} height={180} />
        </ChartCard>
      </div>
    </>
  );
}
