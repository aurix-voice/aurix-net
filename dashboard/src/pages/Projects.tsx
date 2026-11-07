import { useNavigate } from "@tanstack/react-router";
import { Plus } from "lucide-react";
import { useState } from "react";

import type { NewKey, Project } from "@/api/client";
import { useCreateProject, useProjects } from "@/api/hooks";
import { useI18n } from "@/i18n";
import { fmtDateTimeShort } from "@/lib/format";
import { RequireOperator } from "@/shell/Guards";
import { Button } from "@/ui/Button";
import { Dialog, FormDialog } from "@/ui/Dialog";
import { Input } from "@/ui/Input";
import { PageHeader } from "@/ui/Page";
import { Badge, Callout, Card, CodeBlock, Field, IdChip } from "@/ui/Primitives";
import { DataTable, type Column } from "@/ui/Table";

function ProjectsBody() {
  const { t, locale } = useI18n();
  const navigate = useNavigate();
  const list = useProjects();
  const create = useCreateProject();
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [plan, setPlan] = useState("self_hosted");
  const [created, setCreated] = useState<{ project: Project; key: NewKey } | null>(null);
  const cols: Column<Project>[] = [
    { key: "name", header: t("common.name"), sort: (p) => p.name, cell: (p) => <span className="font-medium">{p.name}</span> },
    { key: "id", header: "ID", cell: (p) => <IdChip id={p.id} /> },
    { key: "plan", header: t("projects.plan"), cell: (p) => <span className="flex items-center gap-1.5"><Badge>{p.plan}</Badge>{p.disabled_at ? <Badge tone="warn">{t("projects.suspended")}</Badge> : null}</span> },
    { key: "limits", header: t("projects.limits"), cell: (p) => <span className="text-fg-muted text-xs">{Object.keys(p.limits ?? {}).length ? Object.entries(p.limits).map(([k, v]) => `${k}=${String(v)}`).join(" · ") : t("common.unlimited")}</span> },
    { key: "created", header: t("common.created"), sort: (p) => p.created_at, cell: (p) => <span className="text-fg-muted">{fmtDateTimeShort(locale, p.created_at)}</span> },
  ];
  return (
    <>
      <PageHeader
        title={t("projects.title")}
        description={t("projects.desc")}
        actions={
          <Button onClick={() => setOpen(true)}>
            <Plus className="size-3.5" /> {t("projects.new")}
          </Button>
        }
      />
      <Card>
        <DataTable rows={list.data} columns={cols} rowKey={(p) => p.id} loading={list.isLoading} error={list.error} onRowClick={(p) => void navigate({ to: "/projects/$projectId", params: { projectId: p.id } })} />
      </Card>
      <FormDialog
        open={open}
        onOpenChange={setOpen}
        title={t("projects.new")}
        submitLabel={t("common.create")}
        disabled={!name.trim()}
        onSubmit={async () => {
          const r = await create.mutateAsync({ name: name.trim(), plan: plan.trim() || undefined });
          setOpen(false);
          setName("");
          setCreated(r);
        }}
      >
        <Field label={t("common.name")} htmlFor="pr-name" required>
          <Input id="pr-name" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
        </Field>
        <Field label={t("projects.plan")} htmlFor="pr-plan">
          <Input id="pr-plan" value={plan} onChange={(e) => setPlan(e.target.value)} />
        </Field>
      </FormDialog>
      <Dialog
        open={!!created}
        onOpenChange={(o) => !o && setCreated(null)}
        title={created?.project.name ?? ""}
        description={t("projects.created")}
        footer={
          <>
            <Button variant="secondary" onClick={() => setCreated(null)}>
              {t("common.close")}
            </Button>
            {created ? (
              <Button
                onClick={() => {
                  const id = created.project.id;
                  setCreated(null);
                  void navigate({ to: "/projects/$projectId", params: { projectId: id } });
                }}
              >
                {t("projects.open")}
              </Button>
            ) : null}
          </>
        }
      >
        <Callout tone="warn">{t("keys.created")}</Callout>
        {created ? <CodeBlock value={created.key.key} className="mt-3" /> : null}
      </Dialog>
    </>
  );
}

export function ProjectsPage() {
  return (
    <RequireOperator>
      <ProjectsBody />
    </RequireOperator>
  );
}
