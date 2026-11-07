import { Link, useParams } from "@tanstack/react-router";
import { ArrowLeft, Download, Megaphone } from "lucide-react";
import { useState } from "react";

import type { RoomDetail } from "@/api/client";
import { useAddBan, useKickPlayer, useRoom } from "@/api/hooks";
import { NoticeDialog } from "@/components/NoticeDialog";
import { useAuth } from "@/auth/AuthProvider";
import { useI18n } from "@/i18n";
import { fmtDateTimeShort } from "@/lib/format";
import { modeTone } from "@/lib/rooms";
import { Button } from "@/ui/Button";
import { ConfirmDialog } from "@/ui/Dialog";
import { Input } from "@/ui/Input";
import { Loading, PageHeader, QueryError, Section, SplitLayout } from "@/ui/Page";
import { Badge, Callout, Card, CodeBlock, IdChip, KV, Mono } from "@/ui/Primitives";
import { DataTable } from "@/ui/Table";
import { useToast } from "@/ui/Toast";

export function RoomDetailPage() {
  const { roomId } = useParams({ from: "/shell/rooms/$roomId" });
  const { t, locale } = useI18n();
  const { token, operator } = useAuth();
  const q = useRoom(roomId);
  const toast = useToast();
  const kick = useKickPlayer(roomId);
  const [kicking, setKicking] = useState<RoomDetail["players"][number] | null>(null);
  const [kickReason, setKickReason] = useState("");
  const [alsoBan, setAlsoBan] = useState(false);
  const [noticing, setNoticing] = useState(false);
  const addBan = useAddBan(operator ? (q.data?.room.project_id ?? undefined) : undefined);
  if (q.isLoading) return <Loading />;
  if (q.isError) return <QueryError error={q.error} onRetry={() => void q.refetch()} />;
  const d = q.data!;
  const room = d.room;
  const live = d.live;

  async function downloadReplay() {
    const res = await fetch(operator ? `/v1/operator/rooms/${roomId}/replay` : `/v1/rooms/${roomId}/replay`, { headers: { authorization: `Bearer ${token}` } });
    if (!res.ok) return;
    const blob = await res.blob();
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = `${room.name || room.id}.replay.json`;
    a.click();
    URL.revokeObjectURL(a.href);
  }

  return (
    <>
      <PageHeader
        title={
          <span className="flex items-center gap-3">
            <Link to="/rooms" className="text-fg-muted hover:text-fg">
              <ArrowLeft className="size-4" />
            </Link>
            {room.name || t("rooms.detail.title")}
            <Badge tone={modeTone(room.mode)}>{room.mode}</Badge>
            <Badge tone={room.state === "open" ? "ok" : room.state === "orphaned" ? "warn" : "neutral"}>{room.state}</Badge>
          </span>
        }
        description={<IdChip id={room.id} short={36} />}
        actions={
          <span className="flex gap-2">
            <Button variant="secondary" size="sm" onClick={() => setNoticing(true)}>
              <Megaphone className="size-3.5" /> {t("notice.title")}
            </Button>
            {room.mode === "lockstep" ? (
              <Button variant="secondary" size="sm" onClick={() => void downloadReplay()}>
                <Download className="size-3.5" /> {t("rooms.detail.replay")}
              </Button>
            ) : null}
          </span>
        }
      />
      <NoticeDialog open={noticing} onOpenChange={setNoticing} roomId={room.id} />
      <SplitLayout
        main={
          <>
            {!live ? <Callout tone="warn">{t("rooms.detail.noLive")}</Callout> : null}
            {d.players_source === "unavailable" ? <Callout tone="warn">{t("rooms.detail.playersUnavailable")}</Callout> : null}
            <Section title={t("rooms.detail.players")}>
              <Card>
                <DataTable
                  rows={d.players}
                  rowKey={(p) => String(p.actor)}
                  dense
                  columns={[
                    { key: "actor", header: t("rooms.actor"), cell: (p) => <Mono>{p.actor}</Mono>, width: "80px" },
                    { key: "name", header: t("common.name"), cell: (p) => p.name || "—" },
                    { key: "pid", header: t("rooms.playerId"), cell: (p) => <Mono>{p.player_id}</Mono> },
                    { key: "online", header: t("rooms.state"), cell: (p) => <Badge tone={p.online ? "ok" : "neutral"} dot>{p.online ? t("rooms.online") : t("rooms.offline")}</Badge> },
                    {
                      key: "actions",
                      header: "",
                      align: "right",
                      cell: (p) => (
                        <Button variant="ghost" size="sm" disabled={!live} onClick={() => setKicking(p)}>
                          {t("rooms.kick")}
                        </Button>
                      ),
                    },
                  ]}
                />
              </Card>
            </Section>
            <ConfirmDialog
              open={kicking !== null}
              onOpenChange={(o) => {
                if (!o) {
                  setKicking(null);
                  setKickReason("");
                  setAlsoBan(false);
                }
              }}
              title={t("rooms.kick")}
              description={kicking ? t("rooms.kick.confirm", { name: kicking.name || kicking.player_id, actor: kicking.actor }) : ""}
              confirmLabel={t("rooms.kick")}
              variant="danger"
              onConfirm={async () => {
                if (!kicking) return;
                if (alsoBan) await addBan.mutateAsync({ player_id: kicking.player_id, reason: kickReason.trim() || undefined });
                await kick.mutateAsync({ actor: kicking.actor, reason: kickReason.trim() || undefined });
                toast.ok(t("rooms.kick.done", { name: kicking.name || kicking.player_id }));
              }}
            >
              <Input aria-label={t("rooms.kick.reason")} placeholder={t("rooms.kick.reason")} value={kickReason} maxLength={64} onChange={(e) => setKickReason(e.target.value)} />
              <label className="flex items-center gap-2 text-sm">
                <input type="checkbox" className="size-4 accent-fg" checked={alsoBan} onChange={(e) => setAlsoBan(e.target.checked)} />
                {t("rooms.kick.ban")}
              </label>
            </ConfirmDialog>
            <Section title={t("rooms.detail.props")}>
              <CodeBlock value={JSON.stringify(live?.props ?? room.props, null, 2)} />
            </Section>
          </>
        }
        side={
          <Card className="p-4">
            <KV
              cols={1}
              items={[
                { k: t("rooms.node"), v: room.node_id ? <IdChip id={room.node_id} /> : "—" },
                { k: t("overview.region"), v: room.region },
                { k: t("rooms.players"), v: live ? `${live.players} / ${live.max_players}` : `— / ${room.max_players}` },
                { k: t("rooms.visible"), v: room.visible ? t("common.yes") : t("common.no") },
                { k: t("rooms.schema"), v: room.schema_hash ? <Mono>{room.schema_hash.slice(0, 16)}</Mono> : "—" },
                { k: t("common.created"), v: fmtDateTimeShort(locale, room.created_at) },
                { k: t("rooms.closed"), v: room.closed_at ? fmtDateTimeShort(locale, room.closed_at) : "—" },
              ]}
            />
          </Card>
        }
      />
    </>
  );
}
