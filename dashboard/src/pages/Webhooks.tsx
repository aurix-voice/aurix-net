import { Plus } from "lucide-react";
import { useState } from "react";

import type { Webhook } from "@/api/client";
import { useCreateWebhook, useDeleteWebhook, useWebhooks } from "@/api/hooks";
import { useI18n } from "@/i18n";
import { fmtDateTimeShort } from "@/lib/format";
import { RequireScope } from "@/shell/Guards";
import { Button } from "@/ui/Button";
import { ConfirmDialog, Dialog, FormDialog } from "@/ui/Dialog";
import { Input } from "@/ui/Input";
import { PageHeader } from "@/ui/Page";
import { Badge, Callout, Card, CodeBlock, Field, IdChip, Mono } from "@/ui/Primitives";
import { DataTable, type Column } from "@/ui/Table";

const EVENTS = ["room.created", "room.closed", "player.joined", "player.left", "match.found"];

function WebhooksBody() {
  const { t, locale } = useI18n();
  const list = useWebhooks();
  const create = useCreateWebhook();
  const del = useDeleteWebhook();
  const [open, setOpen] = useState(false);
  const [url, setUrl] = useState("");
  const [events, setEvents] = useState("");
  const [created, setCreated] = useState<Webhook | null>(null);
  const [deleting, setDeleting] = useState<Webhook | null>(null);
  const cols: Column<Webhook>[] = [
    { key: "url", header: t("webhooks.url"), cell: (w) => <Mono className="truncate max-w-[420px] inline-block align-bottom">{w.url}</Mono> },
    { key: "id", header: "ID", cell: (w) => <IdChip id={w.id} /> },
    { key: "events", header: t("webhooks.events"), cell: (w) => (w.events.length ? <span className="flex gap-1 flex-wrap">{w.events.map((e) => <Badge key={e}>{e}</Badge>)}</span> : <span className="text-fg-muted">*</span>) },
    { key: "created", header: t("common.created"), cell: (w) => <span className="text-fg-muted">{w.created_at ? fmtDateTimeShort(locale, w.created_at) : "—"}</span> },
    {
      key: "actions",
      header: "",
      align: "right",
      cell: (w) => (
        <Button variant="ghost" size="sm" onClick={() => setDeleting(w)}>
          {t("common.delete")}
        </Button>
      ),
    },
  ];
  return (
    <>
      <PageHeader
        title={t("webhooks.title")}
        description={t("webhooks.desc")}
        actions={
          <Button onClick={() => setOpen(true)}>
            <Plus className="size-3.5" /> {t("webhooks.new")}
          </Button>
        }
      />
      <Card>
        <DataTable rows={list.data} columns={cols} rowKey={(w) => w.id} loading={list.isLoading} error={list.error} />
      </Card>
      <FormDialog
        open={open}
        onOpenChange={setOpen}
        title={t("webhooks.new")}
        submitLabel={t("common.create")}
        disabled={!url.trim()}
        onSubmit={async () => {
          const w = await create.mutateAsync({ url: url.trim(), events: events.split(",").map((s) => s.trim()).filter(Boolean) });
          setOpen(false);
          setUrl("");
          setEvents("");
          setCreated(w);
        }}
      >
        <Field label={t("webhooks.url")} htmlFor="wh-url" required>
          <Input id="wh-url" type="url" placeholder="https://" value={url} onChange={(e) => setUrl(e.target.value)} autoFocus />
        </Field>
        <Field label={t("webhooks.events")} htmlFor="wh-events" hint={`${t("webhooks.events.hint")} · ${EVENTS.join(", ")}`}>
          <Input id="wh-events" value={events} onChange={(e) => setEvents(e.target.value)} placeholder="room.created, room.closed" />
        </Field>
      </FormDialog>
      <Dialog open={!!created} onOpenChange={(o) => !o && setCreated(null)} title={t("webhooks.new")} footer={<Button onClick={() => setCreated(null)}>{t("common.close")}</Button>}>
        <Callout tone="warn">{t("webhooks.secret")}</Callout>
        {created?.secret ? <CodeBlock value={created.secret} className="mt-3" /> : null}
      </Dialog>
      <ConfirmDialog
        open={!!deleting}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={t("common.delete")}
        description={t("webhooks.delete.confirm")}
        confirmLabel={t("common.delete")}
        variant="danger"
        onConfirm={async () => {
          if (deleting) await del.mutateAsync(deleting.id);
          setDeleting(null);
        }}
      />
    </>
  );
}

export function WebhooksPage() {
  return (
    <RequireScope scope="admin">
      <WebhooksBody />
    </RequireScope>
  );
}
