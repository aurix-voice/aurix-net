import { Plus } from "lucide-react";
import { useState } from "react";

import type { NewOperatorToken, OperatorToken } from "@/api/client";
import { useCreateOperatorToken, useOperatorTokens, useRevokeOperatorToken } from "@/api/hooks";
import { useI18n } from "@/i18n";
import { fmtDateTimeShort, fmtRelative } from "@/lib/format";
import { RequireOperator } from "@/shell/Guards";
import { Button } from "@/ui/Button";
import { ConfirmDialog, Dialog, FormDialog } from "@/ui/Dialog";
import { Input, NativeSelect } from "@/ui/Input";
import { PageHeader } from "@/ui/Page";
import { Badge, Callout, Card, CardHeader, CodeBlock, Field, Mono } from "@/ui/Primitives";
import { DataTable, type Column } from "@/ui/Table";
import { useToast } from "@/ui/Toast";

const EXPIRIES = [0, 30, 90, 365] as const;

function TokensCard() {
  const { t, locale } = useI18n();
  const toast = useToast();
  const list = useOperatorTokens();
  const create = useCreateOperatorToken();
  const revoke = useRevokeOperatorToken();
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [expiryDays, setExpiryDays] = useState(0);
  const [created, setCreated] = useState<NewOperatorToken | null>(null);
  const [revoking, setRevoking] = useState<OperatorToken | null>(null);
  const cols: Column<OperatorToken>[] = [
    { key: "name", header: t("common.name"), cell: (x) => <span className="font-medium">{x.name}</span> },
    { key: "prefix", header: t("keys.prefix"), cell: (x) => <Mono>an_{x.prefix}…</Mono> },
    { key: "by", header: t("audit.actor"), cell: (x) => <Mono>{x.created_by}</Mono> },
    { key: "created", header: t("common.created"), cell: (x) => <span className="text-fg-muted">{fmtDateTimeShort(locale, x.created_at)}</span> },
    { key: "used", header: t("keys.lastUsed"), cell: (x) => <span className="text-fg-muted">{x.last_used_at ? fmtRelative(locale, x.last_used_at) : "—"}</span> },
    { key: "expires", header: t("keys.expires"), cell: (x) => <span className="text-fg-muted">{x.expires_at ? fmtDateTimeShort(locale, x.expires_at) : t("keys.expires.never")}</span> },
    {
      key: "state",
      header: t("rooms.state"),
      cell: (x) =>
        x.revoked_at ? <Badge tone="danger">{t("keys.revoked")}</Badge> : x.expires_at && Date.parse(x.expires_at) < Date.now() ? <Badge tone="warn">{t("keys.expired")}</Badge> : <Badge tone="ok" dot>{t("rooms.open")}</Badge>,
    },
    {
      key: "actions",
      header: "",
      align: "right",
      cell: (x) =>
        x.revoked_at ? null : (
          <Button variant="ghost" size="sm" onClick={() => setRevoking(x)}>
            {t("keys.revoke")}
          </Button>
        ),
    },
  ];
  return (
    <Card>
      <CardHeader
        title={t("settings.tokens")}
        description={t("settings.tokens.desc")}
        actions={
          <Button size="sm" onClick={() => setOpen(true)}>
            <Plus className="size-3.5" /> {t("settings.tokens.new")}
          </Button>
        }
      />
      <DataTable rows={list.data} columns={cols} rowKey={(x) => x.id} loading={list.isLoading} error={list.error} dense empty={<span className="block px-3 py-6 text-center text-sm text-fg-muted">{t("settings.tokens.empty")}</span>} />
      <FormDialog
        open={open}
        onOpenChange={(o) => {
          setOpen(o);
          if (!o) setName("");
        }}
        title={t("settings.tokens.new")}
        description={t("settings.tokens.new.desc")}
        submitLabel={t("common.create")}
        disabled={!name.trim()}
        onSubmit={async () => {
          const r = await create.mutateAsync({ name: name.trim(), expires_in_secs: expiryDays ? expiryDays * 86_400 : undefined });
          setOpen(false);
          setName("");
          setCreated(r);
        }}
      >
        <Field label={t("common.name")} htmlFor="op-token-name" required>
          <Input id="op-token-name" value={name} onChange={(e) => setName(e.target.value)} autoFocus maxLength={64} />
        </Field>
        <Field label={t("keys.expires")} htmlFor="op-token-expiry">
          <NativeSelect id="op-token-expiry" value={String(expiryDays)} onChange={(e) => setExpiryDays(Number(e.target.value))}>
            {EXPIRIES.map((d) => (
              <option key={d} value={d}>
                {d === 0 ? t("keys.expires.never") : t("keys.expires.days", { d })}
              </option>
            ))}
          </NativeSelect>
        </Field>
      </FormDialog>
      <Dialog open={!!created} onOpenChange={(o) => !o && setCreated(null)} title={t("settings.tokens.new")} footer={<Button onClick={() => setCreated(null)}>{t("common.close")}</Button>}>
        <Callout tone="warn">{t("settings.tokens.created")}</Callout>
        {created ? <CodeBlock value={created.token} className="mt-3" /> : null}
      </Dialog>
      <ConfirmDialog
        open={!!revoking}
        onOpenChange={(o) => !o && setRevoking(null)}
        title={t("keys.revoke")}
        description={revoking ? t("settings.tokens.revoke.confirm", { name: revoking.name }) : null}
        confirmLabel={t("keys.revoke")}
        variant="danger"
        onConfirm={async () => {
          if (!revoking) return;
          try {
            await revoke.mutateAsync(revoking.id);
            toast.ok(t("settings.tokens.revoked", { name: revoking.name }));
          } catch (err) {
            toast.error(err instanceof Error ? err.message : String(err));
          }
          setRevoking(null);
        }}
      />
    </Card>
  );
}

export function SettingsPage() {
  const { t } = useI18n();
  return (
    <RequireOperator>
      <PageHeader title={t("settings.title")} description={t("settings.desc")} />
      <TokensCard />
    </RequireOperator>
  );
}
