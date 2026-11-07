import type { Rejections } from "@/api/client";
import { useI18n } from "@/i18n";
import { fmtNumber } from "@/lib/format";
import { Badge, Card } from "@/ui/Primitives";

const REASONS = ["rate_limited", "quota", "capacity", "room_full", "suspended", "banned", "region", "grant"] as const;

/** Compact "who got turned away and why" panel for one project. */
export function RejectionsCard({ data, loading }: { data: Rejections | undefined; loading?: boolean }) {
  const { t, locale } = useI18n();
  const total = data ? Object.values(data.total).reduce((a, b) => a + b, 0) : 0;
  const peak = data ? Math.max(1, ...data.series.map((s) => Object.values(s.counts).reduce((a, b) => a + b, 0))) : 1;
  return (
    <Card className="p-4">
      <div className="flex items-start justify-between gap-3">
        <div>
          <div className="text-xs font-medium text-fg-muted">{t("rejections.title", { h: data?.hours ?? 24 })}</div>
          <div className="mt-1 text-2xl font-semibold tabular tracking-tight">{loading && !data ? "…" : fmtNumber(locale, total)}</div>
        </div>
        <Badge tone={total === 0 ? "ok" : "warn"} dot>
          {total === 0 ? t("rejections.none") : t("rejections.some")}
        </Badge>
      </div>
      {data ? (
        <div className="mt-3 flex h-8 items-end gap-px" aria-hidden>
          {data.series.map((s) => {
            const n = Object.values(s.counts).reduce((a, b) => a + b, 0);
            return <div key={s.t} className={n ? "flex-1 rounded-sm bg-warn/70" : "flex-1 rounded-sm bg-border"} style={{ height: `${Math.max(8, Math.round((n / peak) * 100))}%` }} title={`${s.t}: ${n}`} />;
          })}
        </div>
      ) : null}
      <dl className="mt-3 grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
        {REASONS.map((r) => (
          <div key={r} className="flex items-center justify-between gap-2">
            <dt className="text-fg-muted">{t(`rejections.${r}`)}</dt>
            <dd className="tabular font-medium">{fmtNumber(locale, data?.total[r] ?? 0)}</dd>
          </div>
        ))}
      </dl>
      <div className="mt-2 text-[11px] text-fg-faint">{t("rejections.hint")}</div>
    </Card>
  );
}
