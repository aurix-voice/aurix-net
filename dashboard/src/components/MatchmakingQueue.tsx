import { useMatchmakingStats } from "@/api/hooks";
import { useI18n } from "@/i18n";
import { fmtNumber } from "@/lib/format";
import { modeTone } from "@/lib/rooms";
import { Badge, IdChip, Mono, Stat } from "@/ui/Primitives";
import { DataTable } from "@/ui/Table";

/** Live matchmaking queue of one project: who is waiting, for how long, and how many matches were made. */
export function MatchmakingQueue({ projectId }: { projectId?: string }) {
  const { t, locale } = useI18n();
  const q = useMatchmakingStats(projectId);
  return (
    <div>
      <div className="grid gap-3 sm:grid-cols-3 p-3 border-b border-border">
        <Stat label={t("mm.queue.searching")} value={q.data ? fmtNumber(locale, q.data.searching.length) : "…"} />
        <Stat label={t("mm.queue.matchedHour")} value={q.data ? fmtNumber(locale, q.data.matched.last_hour) : "…"} />
        <Stat label={t("mm.queue.matched24h")} value={q.data ? fmtNumber(locale, q.data.matched.last_24h) : "…"} />
      </div>
      <DataTable
        rows={q.data?.searching}
        rowKey={(x) => x.id}
        dense
        loading={q.isLoading}
        error={q.error}
        empty={<span className="block px-3 py-6 text-center text-sm text-fg-muted">{t("mm.queue.empty")}</span>}
        columns={[
          { key: "id", header: t("mm.ticket"), cell: (x) => <IdChip id={x.id} /> },
          { key: "player", header: t("rooms.playerId"), cell: (x) => <Mono>{x.player_id}</Mono> },
          { key: "mode", header: t("rooms.mode"), cell: (x) => <Badge tone={modeTone(x.mode)}>{x.mode}</Badge> },
          { key: "region", header: t("nodes.region"), cell: (x) => x.region ?? <span className="text-fg-muted">*</span> },
          { key: "size", header: t("mm.size"), align: "right", cell: (x) => x.max_players },
          {
            key: "filter",
            header: t("mm.filter"),
            cell: (x) => (
              <span className="flex flex-wrap items-center gap-1">
                {x.filter ? <Mono>{x.filter}</Mono> : <span className="text-fg-muted">—</span>}
                {x.fill_existing ? <Badge tone="neutral">{t("mm.fill")}{x.match_type ? ` · ${x.match_type}` : ""}</Badge> : null}
              </span>
            ),
          },
          { key: "age", header: t("mm.queue.waiting"), align: "right", cell: (x) => <span className={x.age_secs > 60 ? "text-warn tabular" : "tabular"}>{x.age_secs}s</span> },
        ]}
      />
    </div>
  );
}
