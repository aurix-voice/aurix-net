import { Link } from "@tanstack/react-router";

import type { Attention } from "@/api/client";
import { useI18n } from "@/i18n";
import { fmtNumber, fmtRelative } from "@/lib/format";
import { Badge, Card, Mono } from "@/ui/Primitives";

/** Operator "needs a look" list: empty state means the fleet is quiet. */
export function AttentionPanel({ data, loading }: { data: Attention | undefined; loading?: boolean }) {
  const { t, locale } = useI18n();
  const count = data ? data.stale_nodes.length + data.draining_nodes.length + data.suspended_projects.length + data.failing_webhooks.length + data.rejections.length + (data.usage_caps?.length ?? 0) : 0;
  return (
    <Card className="p-4">
      <div className="flex items-center justify-between gap-3 mb-3">
        <div className="text-sm font-medium">{t("attention.title")}</div>
        <Badge tone={count === 0 ? "ok" : "warn"} dot>
          {loading && !data ? "…" : count === 0 ? t("attention.quiet") : t("attention.items", { n: count })}
        </Badge>
      </div>
      {data && count === 0 ? <div className="text-sm text-fg-muted">{t("attention.quiet.desc")}</div> : null}
      <ul className="space-y-1.5 text-sm min-w-0">
        {data?.stale_nodes.map((n) => (
          <li key={`s-${n.id}`} className="flex items-center justify-between gap-2">
            <span className="flex items-center gap-2 min-w-0">
              <Badge tone="danger" dot>{t("nodes.status.stale")}</Badge>
              <Link to="/nodes" className="truncate hover:underline"><Mono>{n.public_host}:{n.quic_port}</Mono></Link>
            </span>
            <span className="text-xs text-fg-muted whitespace-nowrap">{fmtRelative(locale, n.last_heartbeat)}</span>
          </li>
        ))}
        {data?.draining_nodes.map((n) => (
          <li key={`d-${n.id}`} className="flex items-center justify-between gap-2">
            <span className="flex items-center gap-2 min-w-0">
              <Badge tone="warn" dot>{t("nodes.status.draining")}</Badge>
              <Link to="/nodes" className="truncate hover:underline"><Mono>{n.public_host}:{n.quic_port}</Mono></Link>
            </span>
            <span className="text-xs text-fg-muted whitespace-nowrap">{t("attention.rooms", { n: n.rooms })}</span>
          </li>
        ))}
        {data?.suspended_projects.map((p) => (
          <li key={`p-${p.id}`} className="flex items-center justify-between gap-2">
            <span className="flex items-center gap-2 min-w-0">
              <Badge tone="warn">{t("projects.suspended")}</Badge>
              <Link to="/projects/$projectId" params={{ projectId: p.id }} className="truncate hover:underline">{p.name}</Link>
            </span>
            <span className="text-xs text-fg-muted whitespace-nowrap">{fmtRelative(locale, p.since)}</span>
          </li>
        ))}
        {data?.failing_webhooks.map((w) => (
          <li key={`w-${w.webhook_id}`} className="flex flex-wrap items-center justify-between gap-x-2 gap-y-1">
            <span className="flex items-center gap-2 min-w-0">
              <Badge tone="danger">{t("attention.webhook")}</Badge>
              <Link to="/projects/$projectId" params={{ projectId: w.project_id }} className="truncate hover:underline">{w.project_name || w.project_id.slice(0, 8)}</Link>
              <Mono className="hidden sm:inline truncate text-xs text-fg-muted">{w.url}</Mono>
            </span>
            <span className="text-xs text-fg-muted whitespace-nowrap">{t("attention.failed", { f: fmtNumber(locale, w.failed), n: fmtNumber(locale, w.total) })}</span>
          </li>
        ))}
        {data?.usage_caps?.map((u) => (
          <li key={`u-${u.project_id}-${u.metric}`} className="flex items-center justify-between gap-2">
            <span className="flex items-center gap-2 min-w-0">
              <Badge tone={u.used >= u.cap ? "danger" : "warn"}>{t("attention.usage_cap")}</Badge>
              <Link to="/projects/$projectId" params={{ projectId: u.project_id }} className="truncate hover:underline">{u.project_name}</Link>
            </span>
            <span className="text-xs text-fg-muted whitespace-nowrap">
              {t("attention.usage", { pct: Math.round((u.used / u.cap) * 100), metric: u.metric === "bytes" ? t("health.usage.metric.bytes") : t("health.usage.metric.minutes") })}
            </span>
          </li>
        ))}
        {data?.rejections.map((r) => (
          <li key={`r-${r.project_id}`} className="flex items-center justify-between gap-2">
            <span className="flex items-center gap-2 min-w-0">
              <Badge tone="warn">{t("attention.rejections")}</Badge>
              <Link to="/projects/$projectId" params={{ projectId: r.project_id }} className="truncate hover:underline">{r.project_name}</Link>
            </span>
            <span className="text-xs text-fg-muted whitespace-nowrap" title={Object.entries(r.by).map(([k, v]) => `${k}: ${v}`).join(", ")}>
              {t("attention.rejected", { n: fmtNumber(locale, r.total) })}
            </span>
          </li>
        ))}
      </ul>
    </Card>
  );
}
