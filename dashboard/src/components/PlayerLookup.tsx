import { Link } from "@tanstack/react-router";
import { Search } from "lucide-react";
import { useState } from "react";

import { useMutation } from "@tanstack/react-query";

import { useAddBan, useApi, usePlayerLookup } from "@/api/hooks";
import { useAuth } from "@/auth/AuthProvider";
import { useI18n } from "@/i18n";
import { fmtDateTimeShort } from "@/lib/format";
import { modeTone } from "@/lib/rooms";
import { Button } from "@/ui/Button";
import { Input } from "@/ui/Input";
import { Badge, Callout, IdChip, Mono } from "@/ui/Primitives";
import { DataTable } from "@/ui/Table";
import { useToast } from "@/ui/Toast";

/** Moderation entry point: type a player id, see where they are on the whole fleet, kick or ban from here. */
export function PlayerLookup({ projectId }: { projectId?: string }) {
  const { t, locale } = useI18n();
  const toast = useToast();
  const [draft, setDraft] = useState("");
  const [playerId, setPlayerId] = useState("");
  const q = usePlayerLookup(playerId, projectId);
  const ban = useAddBan(projectId);
  const api = useApi();
  const { operator } = useAuth();
  const kick = useMutation({
    mutationFn: ({ room, actor }: { room: string; actor: number }) =>
      api.request<void>(operator ? `/v1/operator/rooms/${room}/players/${actor}` : `/v1/rooms/${room}/players/${actor}`, { method: "DELETE" }),
  });
  const d = q.data;
  return (
    <div className="p-3 space-y-3">
      <form
        className="flex gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          setPlayerId(draft.trim());
        }}
      >
        <Input aria-label={t("players.id")} placeholder={t("players.placeholder")} value={draft} onChange={(e) => setDraft(e.target.value)} className="max-w-md" />
        <Button type="submit" variant="secondary" disabled={!draft.trim()}>
          <Search className="size-4" />
          {t("players.find")}
        </Button>
      </form>
      {q.error ? <Callout tone="danger">{q.error instanceof Error ? q.error.message : String(q.error)}</Callout> : null}
      {d ? (
        <>
          <div className="flex flex-wrap items-center gap-2 text-sm">
            <Mono>{d.player_id}</Mono>
            {d.ban ? (
              <Badge tone="danger">{d.ban.expires_at ? t("players.bannedUntil", { at: fmtDateTimeShort(locale, d.ban.expires_at) }) : t("players.banned")}</Badge>
            ) : (
              <Badge tone="ok">{t("players.notBanned")}</Badge>
            )}
            <span className="text-fg-muted">{t("players.inRooms", { n: d.rooms.length })}</span>
            {d.unreachable_nodes.length > 0 ? <Badge tone="warn">{t("players.unreachable", { n: d.unreachable_nodes.length })}</Badge> : null}
            {!d.ban ? (
              <Button
                size="sm"
                variant="danger"
                className="ml-auto"
                disabled={ban.isPending}
                onClick={async () => {
                  try {
                    await ban.mutateAsync({ player_id: d.player_id, reason: undefined });
                    toast.ok(t("bans.added", { id: d.player_id }));
                    await q.refetch();
                  } catch (err) {
                    toast.error(err instanceof Error ? err.message : String(err));
                  }
                }}
              >
                {t("bans.add")}
              </Button>
            ) : null}
          </div>
          <DataTable
            rows={d.rooms}
            rowKey={(r) => `${r.room_id}-${r.actor}`}
            dense
            empty={<span className="block px-3 py-4 text-center text-sm text-fg-muted">{t("players.noRooms")}</span>}
            columns={[
              { key: "room", header: t("rooms.title"), cell: (r) => <Link to="/rooms/$roomId" params={{ roomId: r.room_id }} className="font-medium hover:underline">{r.name}</Link> },
              { key: "mode", header: t("rooms.mode"), cell: (r) => <Badge tone={modeTone(r.mode)}>{r.mode}</Badge> },
              { key: "node", header: t("rooms.node"), cell: (r) => <IdChip id={r.node_id} /> },
              { key: "actor", header: t("rooms.actor"), cell: (r) => <Mono>{r.actor}</Mono> },
              { key: "name", header: t("common.name"), cell: (r) => r.player_name || <span className="text-fg-muted">—</span> },
              { key: "online", header: t("rooms.state"), cell: (r) => (r.online ? <Badge tone="ok" dot>{t("rooms.online")}</Badge> : <Badge>{t("rooms.offline")}</Badge>) },
              {
                key: "actions",
                header: "",
                align: "right",
                cell: (r) => (
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={kick.isPending}
                    onClick={async () => {
                      try {
                        await kick.mutateAsync({ room: r.room_id, actor: r.actor });
                        toast.ok(t("rooms.kick.done", { name: r.player_name || d.player_id }));
                        await q.refetch();
                      } catch (err) {
                        toast.error(err instanceof Error ? err.message : String(err));
                      }
                    }}
                  >
                    {t("rooms.kick")}
                  </Button>
                ),
              },
            ]}
          />
          {d.audit.length > 0 ? (
            <div>
              <div className="text-xs uppercase tracking-wide text-fg-muted mb-1">{t("players.history")}</div>
              <ul className="text-sm space-y-1">
                {d.audit.map((a) => (
                  <li key={a.id} className="flex items-center gap-2">
                    <span className="text-fg-muted tabular">{fmtDateTimeShort(locale, a.created_at)}</span>
                    <Mono>{a.action}</Mono>
                    <span className="text-fg-muted truncate">{a.actor}</span>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
        </>
      ) : null}
    </div>
  );
}
