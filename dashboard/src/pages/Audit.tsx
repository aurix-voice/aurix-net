import { baseUrl, type AuditRow } from "@/api/client";
import { Download } from "lucide-react";
import { useAudit } from "@/api/hooks";
import { useAuth } from "@/auth/AuthProvider";
import { useI18n } from "@/i18n";
import { fmtDateTimeShort } from "@/lib/format";
import { RequireScope } from "@/shell/Guards";
import { useState } from "react";

import { Button } from "@/ui/Button";
import { Input, NativeSelect } from "@/ui/Input";
import { PageHeader, Toolbar } from "@/ui/Page";
import { Badge, Card, IdChip, Mono } from "@/ui/Primitives";
import { useToast } from "@/ui/Toast";
import { DataTable, type Column } from "@/ui/Table";

const ACTIONS = ["key.create", "key.revoke", "webhook.create", "webhook.delete", "module.put", "module.delete", "project.create", "project.limits", "room.close"];

function AuditTable() {
  const { t, locale } = useI18n();
  const { operator, projectId, token } = useAuth();
  const toast = useToast();
  const [action, setAction] = useState("");
  const [actor, setActor] = useState("");
  const [limit, setLimit] = useState(200);
  async function exportCsv() {
    const url = new URL(operator ? "/v1/operator/audit" : "/v1/audit", baseUrl());
    if (action.trim()) url.searchParams.set("action", action.trim());
    if (actor.trim()) url.searchParams.set("actor", actor.trim());
    if (operator && projectId) url.searchParams.set("project", projectId);
    url.searchParams.set("limit", String(limit));
    url.searchParams.set("format", "csv");
    try {
      const res = await fetch(url, { headers: token ? { authorization: `Bearer ${token}` } : undefined });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const name = /filename="([^"]+)"/.exec(res.headers.get("content-disposition") ?? "")?.[1] ?? "audit.csv";
      const blob = await res.blob();
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = name;
      a.click();
      URL.revokeObjectURL(a.href);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    }
  }
  const q = useAudit({ action: action.trim(), actor: actor.trim(), limit });
  const cols: Column<AuditRow>[] = [
    { key: "when", header: t("audit.when"), width: "160px", cell: (r) => <span className="text-fg-muted tabular-nums">{fmtDateTimeShort(locale, r.created_at)}</span> },
    { key: "actor", header: t("audit.actor"), cell: (r) => <Mono>{r.actor}</Mono> },
    { key: "action", header: t("audit.action"), cell: (r) => <Badge tone={r.action.endsWith(".revoke") || r.action.endsWith(".delete") ? "danger" : r.action.endsWith(".create") || r.action.endsWith(".put") ? "ok" : "neutral"}>{r.action}</Badge> },
    ...(operator && !projectId ? [{ key: "project", header: t("rooms.project"), cell: (r: AuditRow) => (r.project_id ? <IdChip id={r.project_id} /> : "—") } satisfies Column<AuditRow>] : []),
    { key: "target", header: t("audit.target"), cell: (r) => <IdChip id={r.target} short={12} /> },
    { key: "details", header: t("audit.details"), cell: (r) => <Mono className="text-[11px] text-fg-muted">{Object.keys(r.details).length ? JSON.stringify(r.details) : "—"}</Mono> },
  ];
  return (
    <Card>
      <Toolbar end={<span className="text-xs text-fg-muted tabular-nums">{t("common.total")}: {q.data?.length ?? 0}</span>}>
        <Input placeholder={t("audit.filter.action")} value={action} onChange={(e) => setAction(e.target.value)} className="w-44" list="audit-actions" />
        <datalist id="audit-actions">
          {ACTIONS.map((a) => (
            <option key={a} value={a} />
          ))}
        </datalist>
        <Input placeholder={t("audit.filter.actor")} value={actor} onChange={(e) => setActor(e.target.value)} className="w-44" />
        <NativeSelect aria-label={t("audit.filter.limit")} value={String(limit)} onChange={(e) => setLimit(Number(e.target.value))} className="w-28">
          {[50, 200, 500, 1000].map((n) => (
            <option key={n} value={n}>
              {n}
            </option>
          ))}
        </NativeSelect>
        {action || actor ? (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              setAction("");
              setActor("");
            }}
          >
            {t("rooms.filter.clear")}
          </Button>
        ) : null}
        <Button variant="secondary" size="sm" onClick={() => void exportCsv()}>
          <Download className="size-4" />
          {t("usage.export")}
        </Button>
      </Toolbar>
      <DataTable rows={q.data} columns={cols} rowKey={(r) => String(r.id)} loading={q.isLoading} error={q.error} dense />
    </Card>
  );
}

export function AuditPage() {
  const { t } = useI18n();
  return (
    <RequireScope scope="read">
      <PageHeader title={t("audit.title")} description={t("audit.desc")} />
      <AuditTable />
    </RequireScope>
  );
}
