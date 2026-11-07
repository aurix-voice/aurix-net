import type { AuditRow } from "@/api/client";
import { useAudit } from "@/api/hooks";
import { useAuth } from "@/auth/AuthProvider";
import { useI18n } from "@/i18n";
import { fmtDateTimeShort } from "@/lib/format";
import { RequireScope } from "@/shell/Guards";
import { PageHeader } from "@/ui/Page";
import { Badge, Card, IdChip, Mono } from "@/ui/Primitives";
import { DataTable, type Column } from "@/ui/Table";

function AuditTable() {
  const { t, locale } = useI18n();
  const { operator, projectId } = useAuth();
  const q = useAudit();
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
