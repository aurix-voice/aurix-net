import type { NodeRow } from "@/api/client";
import { useNodeStats } from "@/api/hooks";
import { useI18n } from "@/i18n";
import { fmtBytes, fmtDuration, fmtNumber } from "@/lib/format";
import { Button } from "@/ui/Button";
import { Dialog } from "@/ui/Dialog";
import { Badge, Callout, KV, Mono, Skeleton } from "@/ui/Primitives";

/** What the node is doing right now (asked from the node itself every 3 s). */
export function NodeStatsDialog({ node, onClose }: { node: NodeRow | null; onClose: () => void }) {
  const { t, locale } = useI18n();
  const q = useNodeStats(node?.id ?? null);
  const s = q.data;
  const n = (v: number) => fmtNumber(locale, v);
  const ms = (v: number) => `${v.toFixed(2)} ms`;
  return (
    <Dialog
      open={!!node}
      onOpenChange={(o) => !o && onClose()}
      title={node ? <span className="flex items-center gap-2"><Mono>{node.public_host}:{node.quic_port}</Mono>{s?.draining ? <Badge tone="warn" dot>{t("nodes.status.draining")}</Badge> : s ? <Badge tone="ok" dot>{t("nodes.status.live")}</Badge> : null}</span> : null}
      description={t("nodes.stats.desc")}
      size="lg"
      footer={<Button onClick={onClose}>{t("common.close")}</Button>}
    >
      {q.error ? <Callout tone="danger">{q.error instanceof Error ? q.error.message : String(q.error)}</Callout> : null}
      {!s && !q.error ? <Skeleton className="h-40 w-full" /> : null}
      {s ? (
        <div className="space-y-4">
          <KV
            cols={3}
            items={[
              { k: t("overview.uptime"), v: fmtDuration(locale, s.uptime_secs) },
              { k: t("nodes.version"), v: <Mono>{s.version}</Mono> },
              { k: t("nodes.stats.rss"), v: s.rss_mb == null ? "—" : `${n(s.rss_mb)} MB` },
              { k: t("nodes.rooms"), v: `${n(s.rooms)}${Object.keys(s.rooms_by_mode).length ? ` (${Object.entries(s.rooms_by_mode).map(([m, c]) => `${m} ${c}`).join(", ")})` : ""}` },
              { k: t("nodes.players"), v: `${n(s.players)}${s.spectators ? ` + ${n(s.spectators)} ${t("nodes.stats.spectators")}` : ""}` },
              { k: t("nodes.stats.sessions"), v: n(s.control_sessions) },
            ]}
          />
          <div className="text-xs uppercase tracking-wide text-fg-muted">{t("nodes.stats.traffic")}</div>
          <KV
            cols={2}
            items={[
              { k: t("nodes.stats.datagrams"), v: `${n(s.datagrams_in)} ↓ · ${n(s.datagrams_out)} ↑` },
              { k: t("nodes.stats.bytes"), v: `${fmtBytes(locale, s.bytes_in)} ↓ · ${fmtBytes(locale, s.bytes_out)} ↑` },
            ]}
          />
          <div className="text-xs uppercase tracking-wide text-fg-muted">{t("nodes.stats.simulation")}</div>
          <KV
            cols={3}
            items={[
              { k: t("nodes.stats.tick"), v: s.sync_ticks ? `${ms(s.sync_tick_avg_ms)} · p99 ${ms(s.sync_tick_p99_ms)}` : "—" },
              { k: t("nodes.stats.overruns"), v: <span className={s.tick_overruns ? "text-warn" : ""}>{n(s.tick_overruns)}</span> },
              { k: t("nodes.stats.lostDeltas"), v: n(s.sync_lost_deltas) },
              { k: t("nodes.stats.resyncs"), v: n(s.sync_resyncs) },
              { k: t("nodes.stats.desyncs"), v: <span className={s.lockstep_desyncs ? "text-danger" : ""}>{n(s.lockstep_desyncs)}</span> },
              { k: t("nodes.stats.adopted"), v: n(s.rooms_adopted) },
            ]}
          />
        </div>
      ) : null}
    </Dialog>
  );
}
