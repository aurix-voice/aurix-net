import type { NodeRow } from "@/api/client";
import { useNodes } from "@/api/hooks";
import { useI18n } from "@/i18n";
import { fmtDateTimeShort, fmtNumber, fmtPercent, fmtRelative } from "@/lib/format";
import { nodeStale } from "@/lib/rooms";
import { PageHeader } from "@/ui/Page";
import { Badge, Card, IdChip, Mono, Progress, Tip } from "@/ui/Primitives";
import { DataTable, type Column } from "@/ui/Table";

export function NodesPage() {
  const { t, locale } = useI18n();
  const nodes = useNodes();
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
    { key: "rooms", header: t("nodes.rooms"), align: "right", sort: (n) => n.rooms, cell: (n) => fmtNumber(locale, n.rooms) },
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
      <Card>
        <DataTable rows={nodes.data} columns={cols} rowKey={(n) => n.id} loading={nodes.isLoading} error={nodes.error} />
      </Card>
    </>
  );
}
