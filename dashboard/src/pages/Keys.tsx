import { Plus } from "lucide-react";
import { useState } from "react";

import type { ApiKey } from "@/api/client";
import { useCreateKey, useKeys, useRevokeKey } from "@/api/hooks";
import { NewKeyDialog } from "@/components/NewKeyDialog";
import { useI18n } from "@/i18n";
import { fmtDateTimeShort, fmtRelative } from "@/lib/format";
import { RequireScope } from "@/shell/Guards";
import { Button } from "@/ui/Button";
import { ConfirmDialog } from "@/ui/Dialog";
import { PageHeader } from "@/ui/Page";
import { Badge, Card, Mono } from "@/ui/Primitives";
import { DataTable, type Column } from "@/ui/Table";

export function keyColumns(t: (k: never, v?: never) => string, locale: "en" | "ru", onRevoke?: (k: ApiKey) => void): Column<ApiKey>[] {
  const tt = t as unknown as (k: string) => string;
  return [
    { key: "name", header: tt("common.name"), sort: (k) => k.name, cell: (k) => <span className="font-medium">{k.name}</span> },
    { key: "prefix", header: tt("keys.prefix"), cell: (k) => <Mono>an_{k.prefix}…</Mono> },
    { key: "scopes", header: tt("keys.scopes"), cell: (k) => <span className="flex gap-1 flex-wrap">{k.scopes.map((s) => <Badge key={s}>{s}</Badge>)}</span> },
    { key: "created", header: tt("common.created"), sort: (k) => k.created_at, cell: (k) => <span className="text-fg-muted">{fmtDateTimeShort(locale, k.created_at)}</span> },
    { key: "used", header: tt("keys.lastUsed"), sort: (k) => k.last_used_at ?? "", cell: (k) => <span className="text-fg-muted">{k.last_used_at ? fmtRelative(locale, k.last_used_at) : "—"}</span> },
    {
      key: "state",
      header: tt("rooms.state"),
      cell: (k) => (k.revoked_at ? <Badge tone="danger">{tt("keys.revoked")}</Badge> : <Badge tone="ok" dot>{tt("rooms.open")}</Badge>),
    },
    ...(onRevoke
      ? [
          {
            key: "actions",
            header: "",
            align: "right",
            cell: (k: ApiKey) =>
              k.revoked_at ? null : (
                <Button variant="ghost" size="sm" onClick={() => onRevoke(k)}>
                  {tt("keys.revoke")}
                </Button>
              ),
          } satisfies Column<ApiKey>,
        ]
      : []),
  ];
}

function KeysBody() {
  const { t, locale } = useI18n();
  const keys = useKeys();
  const create = useCreateKey();
  const revoke = useRevokeKey();
  const [open, setOpen] = useState(false);
  const [revoking, setRevoking] = useState<ApiKey | null>(null);
  return (
    <>
      <PageHeader
        title={t("keys.title")}
        description={t("keys.desc")}
        actions={
          <Button onClick={() => setOpen(true)}>
            <Plus className="size-3.5" /> {t("keys.new")}
          </Button>
        }
      />
      <Card>
        <DataTable rows={keys.data} columns={keyColumns(t, locale, setRevoking)} rowKey={(k) => k.id} loading={keys.isLoading} error={keys.error} />
      </Card>
      <NewKeyDialog open={open} onOpenChange={setOpen} create={(b) => create.mutateAsync(b)} />
      <ConfirmDialog
        open={!!revoking}
        onOpenChange={(o) => !o && setRevoking(null)}
        title={t("keys.revoke")}
        description={revoking ? t("keys.revoke.confirm", { name: revoking.name }) : ""}
        confirmLabel={t("keys.revoke")}
        variant="danger"
        onConfirm={async () => {
          if (revoking) await revoke.mutateAsync(revoking.id);
          setRevoking(null);
        }}
      />
    </>
  );
}

export function KeysPage() {
  return (
    <RequireScope scope="admin" projectKey>
      <KeysBody />
    </RequireScope>
  );
}
