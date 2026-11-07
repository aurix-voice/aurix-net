import { Download } from "lucide-react";
import { useMemo, useState } from "react";

import { baseUrl, type Usage as UsageData } from "@/api/client";
import { useRejections, useUsage } from "@/api/hooks";
import { ChartCard, TimeSeriesChart, type Point, type SeriesDef } from "@/components/charts";
import { RangePicker, useRange } from "@/components/RangePicker";
import { useI18n } from "@/i18n";
import { fmtBytes, fmtNumber } from "@/lib/format";
import { useAuth } from "@/auth/AuthProvider";
import { RequireScope } from "@/shell/Guards";
import { Button } from "@/ui/Button";
import { PageHeader } from "@/ui/Page";
import { useToast } from "@/ui/Toast";
import { BillingCard } from "@/components/BillingCard";
import { RejectionsCard } from "@/components/RejectionsCard";
import { Stat } from "@/ui/Primitives";

type Metric = "room_minutes" | "player_minutes" | "bytes_in" | "bytes_out";

export function toPoints(u: UsageData | undefined): Point[] {
  if (!u) return [];
  const byHour = new Map<number, Point>();
  for (const r of u.usage) {
    const t = Date.parse(r.hour);
    const p = byHour.get(t) ?? { t };
    p[r.metric] = (p[r.metric] ?? 0) + r.quantity;
    byHour.set(t, p);
  }
  return [...byHour.values()].sort((a, b) => a.t - b.t);
}

export function totals(u: UsageData | undefined): Record<Metric, number> {
  const out: Record<Metric, number> = { room_minutes: 0, player_minutes: 0, bytes_in: 0, bytes_out: 0 };
  for (const r of u?.usage ?? []) if (r.metric in out) out[r.metric as Metric] += r.quantity;
  return out;
}

function UsageBody() {
  const { t, locale } = useI18n();
  const [range, setRange] = useRange("7d");
  const { operator, projectId, token } = useAuth();
  const toast = useToast();
  const [exporting, setExporting] = useState(false);
  const q = useUsage(range.from, range.to);
  async function exportCsv() {
    const path = operator ? (projectId ? `/v1/operator/projects/${projectId}/usage` : "/v1/operator/usage") : "/v1/usage";
    const url = new URL(path, baseUrl());
    url.searchParams.set("from", range.from);
    url.searchParams.set("to", range.to);
    url.searchParams.set("format", "csv");
    setExporting(true);
    try {
      const res = await fetch(url, { headers: token ? { authorization: `Bearer ${token}` } : undefined });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const name = /filename="([^"]+)"/.exec(res.headers.get("content-disposition") ?? "")?.[1] ?? "usage.csv";
      const blob = await res.blob();
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = name;
      a.click();
      URL.revokeObjectURL(a.href);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    } finally {
      setExporting(false);
    }
  }
  const rejections = useRejections(24);
  const points = useMemo(() => toPoints(q.data), [q.data]);
  const sum = useMemo(() => totals(q.data), [q.data]);
  const minutes: SeriesDef[] = [
    { key: "room_minutes", label: t("usage.room_minutes"), color: "var(--fg)", area: true },
    { key: "player_minutes", label: t("usage.player_minutes"), color: "var(--accent)" },
  ];
  const bytes: SeriesDef[] = [
    { key: "bytes_in", label: t("usage.bytes_in"), color: "var(--fg)", format: (v) => fmtBytes(locale, v) },
    { key: "bytes_out", label: t("usage.bytes_out"), color: "var(--accent)", format: (v) => fmtBytes(locale, v) },
  ];
  return (
    <>
      <PageHeader title={operator && !projectId ? t("usage.fleetTitle") : t("usage.title")} description={t("usage.desc")} actions={
          <div className="flex items-center gap-2">
            <RangePicker value={range} onChange={setRange} />
            <Button variant="secondary" size="sm" disabled={exporting} onClick={() => void exportCsv()}>
              <Download className="size-4" />
              {t("usage.export")}
            </Button>
          </div>
        }
      />
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4 mb-5">
        <Stat label={t("usage.room_minutes")} value={fmtNumber(locale, Math.round(sum.room_minutes))} hint={t("usage.total")} />
        <Stat label={t("usage.player_minutes")} value={fmtNumber(locale, Math.round(sum.player_minutes))} hint={t("usage.total")} />
        <Stat label={t("usage.bytes_in")} value={fmtBytes(locale, sum.bytes_in)} hint={t("usage.total")} />
        <Stat label={t("usage.bytes_out")} value={fmtBytes(locale, sum.bytes_out)} hint={t("usage.total")} />
      </div>
      {!operator || projectId ? (
        <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_360px] mb-5">
          <div />
          <RejectionsCard data={rejections.data} loading={rejections.isLoading} />
        </div>
      ) : null}
      {operator && !projectId ? (
        <div className="mb-5">
          <BillingCard />
        </div>
      ) : null}
      <div className="grid gap-4 xl:grid-cols-2">
        <ChartCard title={`${t("usage.room_minutes")} · ${t("usage.player_minutes")}`} description={t("usage.desc")} series={minutes} query={q}>
          <TimeSeriesChart data={points} series={minutes} empty={t("common.empty")} />
        </ChartCard>
        <ChartCard title={`${t("usage.bytes_in")} · ${t("usage.bytes_out")}`} description={t("usage.desc")} series={bytes} query={q}>
          <TimeSeriesChart data={points} series={bytes} empty={t("common.empty")} />
        </ChartCard>
      </div>
    </>
  );
}

export function UsagePage() {
  return (
    <RequireScope scope="read">
      <UsageBody />
    </RequireScope>
  );
}
