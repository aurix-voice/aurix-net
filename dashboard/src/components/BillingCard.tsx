import { Download } from "lucide-react";
import { useState } from "react";

import { baseUrl, type BillingProject } from "@/api/client";
import { useBilling } from "@/api/hooks";
import { useAuth } from "@/auth/AuthProvider";
import { useI18n } from "@/i18n";
import { fmtBytes, fmtNumber } from "@/lib/format";
import { Button } from "@/ui/Button";
import { NativeSelect } from "@/ui/Input";
import { Badge, Card, CardHeader, Mono } from "@/ui/Primitives";
import { DataTable, type Column } from "@/ui/Table";
import { useToast } from "@/ui/Toast";

function lastMonths(n: number): string[] {
  const out: string[] = [];
  const d = new Date();
  for (let i = 0; i < n; i++) {
    const y = d.getUTCFullYear();
    const m = d.getUTCMonth() + 1;
    out.push(`${y}-${String(m).padStart(2, "0")}`);
    d.setUTCMonth(d.getUTCMonth() - 1, 1);
  }
  return out;
}

/** Month report per project — the numbers an invoice is built from (operator, whole fleet). */
export function BillingCard() {
  const { t, locale } = useI18n();
  const { token } = useAuth();
  const toast = useToast();
  const months = lastMonths(12);
  const [month, setMonth] = useState(months[0] ?? "");
  const q = useBilling(month);
  const [exporting, setExporting] = useState(false);
  async function exportCsv() {
    const url = new URL("/v1/operator/billing", baseUrl());
    url.searchParams.set("month", month);
    url.searchParams.set("format", "csv");
    setExporting(true);
    try {
      const res = await fetch(url, { headers: token ? { authorization: `Bearer ${token}` } : undefined });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const blob = await res.blob();
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = `aurix-net-billing-${month}.csv`;
      a.click();
      URL.revokeObjectURL(a.href);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    } finally {
      setExporting(false);
    }
  }
  const cols: Column<BillingProject>[] = [
    {
      key: "name",
      header: t("common.name"),
      cell: (p) => (
        <span className="flex items-center gap-2">
          <span className="font-medium">{p.name}</span>
          {p.deleted_at ? <Badge tone="neutral">{t("billing.deleted")}</Badge> : p.disabled_at ? <Badge tone="warn">{t("health.suspended")}</Badge> : null}
        </span>
      ),
    },
    { key: "plan", header: t("projects.plan"), cell: (p) => <Mono>{p.plan}</Mono> },
    { key: "pm", header: t("usage.player_minutes"), align: "right", cell: (p) => <span className="tabular">{fmtNumber(locale, Math.round(p.player_minutes))}{p.cap_player_minutes ? <span className="text-fg-muted"> / {fmtNumber(locale, p.cap_player_minutes)}</span> : null}</span> },
    { key: "rm", header: t("usage.room_minutes"), align: "right", cell: (p) => <span className="tabular">{fmtNumber(locale, Math.round(p.room_minutes))}</span> },
    { key: "bytes", header: t("billing.traffic"), align: "right", cell: (p) => <span className="tabular">{fmtBytes(locale, p.bytes_in + p.bytes_out)}{p.cap_bytes ? <span className="text-fg-muted"> / {fmtBytes(locale, p.cap_bytes)}</span> : null}</span> },
    { key: "rooms", header: t("billing.rooms"), align: "right", cell: (p) => <span className="tabular">{fmtNumber(locale, p.rooms_created)}</span> },
  ];
  const tot = q.data?.totals ?? {};
  return (
    <Card data-testid="billing">
      <CardHeader
        title={t("billing.title")}
        description={t("billing.desc")}
        actions={
          <span className="flex items-center gap-2">
            <NativeSelect value={month} onChange={(e) => setMonth(e.target.value)} aria-label={t("billing.month")}>
              {months.map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </NativeSelect>
            <Button variant="secondary" size="sm" disabled={exporting} onClick={() => void exportCsv()}>
              <Download className="size-4" /> {t("usage.export")}
            </Button>
          </span>
        }
      />
      <DataTable rows={q.data?.projects} columns={cols} rowKey={(p) => p.project_id} loading={q.isLoading} error={q.error} dense empty={<span className="block px-3 py-6 text-center text-sm text-fg-muted">{t("common.empty")}</span>} />
      {q.data ? (
        <p className="px-4 py-2 text-xs text-fg-muted border-t border-border">
          {t("billing.totals", { pm: fmtNumber(locale, Math.round(tot.player_minutes ?? 0)), rm: fmtNumber(locale, Math.round(tot.room_minutes ?? 0)), bytes: fmtBytes(locale, (tot.bytes_in ?? 0) + (tot.bytes_out ?? 0)), rooms: fmtNumber(locale, tot.rooms_created ?? 0) })}
        </p>
      ) : null}
    </Card>
  );
}
