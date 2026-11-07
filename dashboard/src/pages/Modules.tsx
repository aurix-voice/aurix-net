import { Upload } from "lucide-react";
import { useState } from "react";

import type { Module } from "@/api/client";
import { useDeleteModule, useModules, usePutModule } from "@/api/hooks";
import { useI18n } from "@/i18n";
import { fmtBytes, fmtDateTimeShort } from "@/lib/format";
import { RequireScope } from "@/shell/Guards";
import { Button } from "@/ui/Button";
import { ConfirmDialog, FormDialog } from "@/ui/Dialog";
import { Input } from "@/ui/Input";
import { PageHeader } from "@/ui/Page";
import { Card, Field, Mono } from "@/ui/Primitives";
import { DataTable, type Column } from "@/ui/Table";

function ModulesBody() {
  const { t, locale } = useI18n();
  const list = useModules();
  const put = usePutModule();
  const del = useDeleteModule();
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [deleting, setDeleting] = useState<Module | null>(null);
  const cols: Column<Module>[] = [
    { key: "name", header: t("common.name"), sort: (m) => m.name, cell: (m) => <span className="font-medium">{m.name}</span> },
    { key: "hash", header: t("modules.hash"), cell: (m) => <Mono title={m.hash}>{m.hash.slice(0, 16)}…</Mono> },
    { key: "size", header: t("modules.size"), align: "right", sort: (m) => m.size, cell: (m) => fmtBytes(locale, m.size) },
    { key: "updated", header: t("modules.updated"), sort: (m) => m.updated_at, cell: (m) => <span className="text-fg-muted">{fmtDateTimeShort(locale, m.updated_at)}</span> },
    {
      key: "actions",
      header: "",
      align: "right",
      cell: (m) => (
        <Button variant="ghost" size="sm" onClick={() => setDeleting(m)}>
          {t("common.delete")}
        </Button>
      ),
    },
  ];
  return (
    <>
      <PageHeader
        title={t("modules.title")}
        description={t("modules.desc")}
        actions={
          <Button onClick={() => setOpen(true)}>
            <Upload className="size-3.5" /> {t("modules.upload")}
          </Button>
        }
      />
      <Card>
        <DataTable rows={list.data} columns={cols} rowKey={(m) => m.name} loading={list.isLoading} error={list.error} />
      </Card>
      <FormDialog
        open={open}
        onOpenChange={setOpen}
        title={t("modules.upload")}
        submitLabel={t("modules.upload")}
        disabled={!name.trim() || !file}
        onSubmit={async () => {
          if (!file) return;
          await put.mutateAsync({ name: name.trim(), file });
          setOpen(false);
          setName("");
          setFile(null);
        }}
      >
        <Field label={t("common.name")} htmlFor="mod-name" required>
          <Input id="mod-name" value={name} onChange={(e) => setName(e.target.value)} placeholder="arena" autoFocus />
        </Field>
        <Field label={t("modules.file")} htmlFor="mod-file" required>
          <Input id="mod-file" type="file" accept=".wasm,application/wasm" onChange={(e) => setFile(e.target.files?.[0] ?? null)} />
        </Field>
      </FormDialog>
      <ConfirmDialog
        open={!!deleting}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={t("common.delete")}
        description={deleting ? t("modules.delete.confirm", { name: deleting.name }) : ""}
        confirmLabel={t("common.delete")}
        variant="danger"
        onConfirm={async () => {
          if (deleting) await del.mutateAsync(deleting.name);
          setDeleting(null);
        }}
      />
    </>
  );
}

export function ModulesPage() {
  return (
    <RequireScope scope="admin">
      <ModulesBody />
    </RequireScope>
  );
}
