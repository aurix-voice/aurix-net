import { Link } from "@tanstack/react-router";

import type { NodeRow } from "@/api/client";
import { useNodes, useRegions } from "@/api/hooks";
import { useAuth } from "@/auth/AuthProvider";
import { useI18n } from "@/i18n";
import { fmtDateTimeShort, fmtNumber, fmtPercent, fmtRelative } from "@/lib/format";
import { nodeStale } from "@/lib/rooms";
import { PageHeader } from "@/ui/Page";
import { Badge, Card, IdChip, Mono, Progress, Tip } from "@/ui/Primitives";
import { DataTable, type Column } from "@/ui/Table";

export function NodesPage() {
  const { t, locale } = useI18n();
  const nodes = useNodes();
  const regions = useRegions();
  const { selectProject } = useAuth();
  const cols: Column<NodeRow>[] = [
    {
      key: "host",
      header: t("nodes.host"),
      sort: (n) => n.public_host,
      cell: (n) => (
        <div className="min-w-0">
          <Mono>
            {n.public_host}:{n.quic_port}
          </Mono>
          <div className="text-[11px] text-fg-faint truncate">{n.http_url}</div>
        </div>
      ),
    },
    { key: "id", header: "ID", cell: (n) => <IdChip id={n.id} /> },
    { key: "region", header: t("nodes.region"), sort: (n) => n.region, cell: (n) => n.region },
    { key: "version", header: t("nodes.version"), cell: (n) => <Mono>{n.version || "—"}</Mono> },
    {
      key: "rooms",
      header: t("nodes.rooms"),
      align: "right",
      sort: (n) => n.rooms,
      cell: (n) =>
        n.rooms > 0 ? (
          <Link to="/rooms" search={{ node: n.id }} className="tabular-nums underline-offset-2 hover:underline"
            onClick={(e) => {
              e.stopPropagation();
              selectProject(null);
            }}
          >
            {fmtNumber(locale, n.rooms)}
          </Link>
        ) : (
          fmtNumber(locale, n.rooms)
        ),
    },
    { key: "players", header: t("nodes.players"), align: "right", sort: (n) => n.players, cell: (n) => fmtNumber(locale, n.players) },
    {
      key: "load",
      header: t("nodes.load"),
      sort: (n) => n.load_factor,
      width: "140px",
      cell: (n) => (
        <div className="flex items-center gap-2">
          <Progress value={n.load_factor} max={1} tone={n.load_factor > 0.85 ? "danger" : n.load_factor > 0.6 ? "warn" : "ok"} className="w-16" />
          <span className="tabular-nums text-fg-muted text-xs">{fmtPercent(locale, n.load_factor)}</span>
        </div>
      ),
    },
    {
      key: "status",
      header: t("nodes.status"),
      cell: (n) => (n.draining ? <Badge tone="warn" dot>{t("nodes.status.draining")}</Badge> : nodeStale(n.last_heartbeat) ? <Badge tone="danger" dot>{t("nodes.status.stale")}</Badge> : <Badge tone="ok" dot>{t("nodes.status.live")}</Badge>),
    },
    {
      key: "hb",
      header: t("nodes.heartbeat"),
      sort: (n) => n.last_heartbeat,
      cell: (n) => (
        <Tip content={fmtDateTimeShort(locale, n.last_heartbeat)}>
          <span className="text-fg-muted">{n.last_heartbeat ? fmtRelative(locale, n.last_heartbeat) : "—"}</span>
        </Tip>
      ),
    },
    { key: "cert", header: t("nodes.cert"), cell: (n) => <Mono title={n.cert_sha256}>{n.cert_sha256.slice(0, 12)}…</Mono> },
  ];
  return (
    <>
      <PageHeader title={t("nodes.title")} description={t("nodes.desc")} />
      {regions.data && regions.data.length > 0 ? (
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          {regions.data.map((r) => (
            <Card key={r.id} className="p-4">
              <div className="flex items-center justify-between gap-2">
                <div className="font-medium truncate">{r.display_name || r.id}</div>
                <Badge tone={r.nodes > 0 ? "ok" : "neutral"} dot>
                  {t("nodes.region.live", { n: r.nodes })}
                </Badge>
              </div>
              <div className="mt-2 space-y-0.5">
                {r.endpoints.map((e) => (
                  <div key={e} className="text-[11px] text-fg-faint truncate font-mono">
                    {e}
                  </div>
                ))}
              </div>
            </Card>
          ))}
        </div>
      ) : null}
      <Card>
        <DataTable rows={nodes.data} columns={cols} rowKey={(n) => n.id} loading={nodes.isLoading} error={nodes.error} />
      </Card>
    </>
  );
}
