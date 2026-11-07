import { useNavigate } from "@tanstack/react-router";
import { DoorOpen } from "lucide-react";
import { useState } from "react";

import type { LiveRoom } from "@/api/client";
import { useCloseRoom, useRooms } from "@/api/hooks";
import { useAuth } from "@/auth/AuthProvider";
import { useI18n } from "@/i18n";
import { fmtNumber } from "@/lib/format";
import { modeTone } from "@/lib/rooms";
import { Button } from "@/ui/Button";
import { ConfirmDialog } from "@/ui/Dialog";
import { Input } from "@/ui/Input";
import { PageHeader, Toolbar } from "@/ui/Page";
import { Badge, Card, EmptyState, IdChip } from "@/ui/Primitives";
import { DataTable, type Column } from "@/ui/Table";
import { useToast } from "@/ui/Toast";

export function RoomsPage() {
  const { t, locale } = useI18n();
  const { operator, can, projectId } = useAuth();
  const navigate = useNavigate();
  const toast = useToast();
  const rooms = useRooms();
  const close = useCloseRoom();
  const [q, setQ] = useState("");
  const [closing, setClosing] = useState<LiveRoom | null>(null);
  const canClose = !operator && can("rooms");

  const rows = (rooms.data ?? []).filter((r) => !q || r.name.toLowerCase().includes(q.toLowerCase()) || r.room.startsWith(q) || r.mode === q);
  const cols: Column<LiveRoom>[] = [
    {
      key: "name",
      header: t("common.name"),
      sort: (r) => r.name,
      cell: (r) => (
        <div className="min-w-0">
          <div className="font-medium truncate">{r.name || "—"}</div>
          <IdChip id={r.room} />
        </div>
      ),
    },
    { key: "mode", header: t("rooms.mode"), sort: (r) => r.mode, cell: (r) => <Badge tone={modeTone(r.mode)}>{r.mode}</Badge> },
    {
      key: "players",
      header: t("rooms.players"),
      align: "right",
      sort: (r) => r.players,
      cell: (r) => (
        <span className="tabular-nums">
          {fmtNumber(locale, r.players)} / {fmtNumber(locale, r.max_players)}
        </span>
      ),
    },
    { key: "node", header: t("rooms.node"), cell: (r) => <IdChip id={r.node} /> },
    ...(operator && !projectId ? [{ key: "project", header: t("rooms.project"), cell: (r: LiveRoom) => (r.project_name ? r.project_name : r.project_id ? <IdChip id={r.project_id} /> : "—") } satisfies Column<LiveRoom>] : []),
    {
      key: "state",
      header: t("rooms.state"),
      cell: (r) => (
        <span className="flex gap-1">
          <Badge tone={r.open ? "ok" : "neutral"}>{r.open ? t("rooms.open") : t("rooms.closed")}</Badge>
          {!r.visible ? <Badge>{t("rooms.hidden")}</Badge> : null}
        </span>
      ),
    },
    ...(canClose
      ? [
          {
            key: "actions",
            header: "",
            align: "right",
            cell: (r: LiveRoom) => (
              <Button
                variant="ghost"
                size="sm"
                onClick={(e) => {
                  e.stopPropagation();
                  setClosing(r);
                }}
              >
                {t("rooms.close")}
              </Button>
            ),
          } satisfies Column<LiveRoom>,
        ]
      : []),
  ];

  return (
    <>
      <PageHeader title={t("rooms.title")} description={t("rooms.desc")} />
      <Card>
        <Toolbar end={<span className="text-xs text-fg-muted tabular-nums">{t("common.total")}: {fmtNumber(locale, rows.length)}</span>}>
          <Input placeholder={t("common.search")} value={q} onChange={(e) => setQ(e.target.value)} className="w-56" />
        </Toolbar>
        <DataTable
          rows={rows}
          columns={cols}
          rowKey={(r) => r.room}
          loading={rooms.isLoading}
          error={rooms.error}
          onRowClick={(r) => void navigate({ to: "/rooms/$roomId", params: { roomId: r.room } })}
          empty={<EmptyState compact icon={<DoorOpen className="size-5" />} title={t("rooms.empty")} description={t("rooms.empty.desc")} />}
        />
      </Card>
      <ConfirmDialog
        open={!!closing}
        onOpenChange={(o) => !o && setClosing(null)}
        title={t("rooms.close")}
        description={closing ? t("rooms.close.confirm", { name: closing.name || closing.room }) : ""}
        confirmLabel={t("rooms.close")}
        variant="danger"
        onConfirm={async () => {
          if (!closing) return;
          await close.mutateAsync(closing.room);
          toast.ok(t("rooms.closed.toast"));
          setClosing(null);
        }}
      />
    </>
  );
}
