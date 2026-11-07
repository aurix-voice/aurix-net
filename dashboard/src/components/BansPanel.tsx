import { Plus } from "lucide-react";
import { useState } from "react";

import type { Ban } from "@/api/client";
import { useAddBan, useBans, useRemoveBan } from "@/api/hooks";
import { useI18n } from "@/i18n";
import { fmtDateTimeShort } from "@/lib/format";
import { Button } from "@/ui/Button";
import { ConfirmDialog, FormDialog } from "@/ui/Dialog";
import { Input, NativeSelect } from "@/ui/Input";
import { Field, Mono } from "@/ui/Primitives";
import { DataTable } from "@/ui/Table";
import { useToast } from "@/ui/Toast";

const DURATIONS = [0, 3600, 86_400, 7 * 86_400, 30 * 86_400] as const;

/** Deny-list of player ids for one project: banned players get 403 at grant issue and at join. */
export function BansPanel({ projectId }: { projectId?: string }) {
  const { t, locale } = useI18n();
  const toast = useToast();
  const bans = useBans(projectId);
  const add = useAddBan(projectId);
  const remove = useRemoveBan(projectId);
  const [adding, setAdding] = useState(false);
  const [playerId, setPlayerId] = useState("");
  const [reason, setReason] = useState("");
  const [duration, setDuration] = useState<number>(0);
  const [removing, setRemoving] = useState<Ban | null>(null);
  return (
    <>
      <div className="flex justify-end px-3 py-2 border-b border-border">
        <Button size="sm" onClick={() => setAdding(true)}>
          <Plus className="size-3.5" /> {t("bans.add")}
        </Button>
      </div>
      <DataTable
        rows={bans.data}
        rowKey={(b) => b.player_id}
        dense
        loading={bans.isLoading}
        error={bans.error}
        empty={<span className="block px-3 py-6 text-center text-sm text-fg-muted">{t("bans.empty")}</span>}
        columns={[
          { key: "player", header: t("rooms.playerId"), cell: (b) => <Mono>{b.player_id}</Mono> },
          { key: "reason", header: t("bans.reason"), cell: (b) => b.reason || <span className="text-fg-muted">—</span> },
          { key: "by", header: t("audit.actor"), cell: (b) => <Mono className="text-xs">{b.created_by}</Mono> },
          { key: "until", header: t("bans.until"), cell: (b) => (b.expires_at ? fmtDateTimeShort(locale, b.expires_at) : <span className="text-fg-muted">{t("bans.permanent")}</span>) },
          { key: "created", header: t("common.created"), cell: (b) => <span className="text-fg-muted">{fmtDateTimeShort(locale, b.created_at)}</span> },
          {
            key: "actions",
            header: "",
            align: "right",
            cell: (b) => (
              <Button variant="ghost" size="sm" onClick={() => setRemoving(b)}>
                {t("bans.remove")}
              </Button>
            ),
          },
        ]}
      />
      <FormDialog
        open={adding}
        onOpenChange={(o) => {
          setAdding(o);
          if (!o) {
            setPlayerId("");
            setReason("");
            setDuration(0);
          }
        }}
        title={t("bans.add")}
        description={t("bans.add.desc")}
        submitLabel={t("bans.add")}
        disabled={!playerId.trim()}
        onSubmit={async () => {
          await add.mutateAsync({ player_id: playerId.trim(), reason: reason.trim() || undefined, duration_secs: duration || undefined });
          toast.ok(t("bans.added", { id: playerId.trim() }));
        }}
      >
        <Field label={t("rooms.playerId")}>
          <Input value={playerId} onChange={(e) => setPlayerId(e.target.value)} maxLength={128} autoFocus />
        </Field>
        <Field label={t("bans.reason")}>
          <Input value={reason} onChange={(e) => setReason(e.target.value)} maxLength={256} />
        </Field>
        <Field label={t("bans.duration")}>
          <NativeSelect value={String(duration)} onChange={(e) => setDuration(Number(e.target.value))}>
            {DURATIONS.map((d) => (
              <option key={d} value={d}>
                {d === 0 ? t("bans.permanent") : t("bans.duration.secs", { h: d / 3600 })}
              </option>
            ))}
          </NativeSelect>
        </Field>
      </FormDialog>
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(o) => !o && setRemoving(null)}
        title={t("bans.remove")}
        description={removing ? t("bans.remove.confirm", { id: removing.player_id }) : ""}
        confirmLabel={t("bans.remove")}
        onConfirm={async () => {
          if (!removing) return;
          await remove.mutateAsync(removing.player_id);
          toast.ok(t("bans.removed", { id: removing.player_id }));
        }}
      />
    </>
  );
}
