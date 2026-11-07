import { useState } from "react";

import { useIssueTestGrant } from "@/api/hooks";
import { useI18n } from "@/i18n";
import { fmtDateTimeShort } from "@/lib/format";
import { Checkbox } from "@/ui/Input";
import { Dialog, FormDialog } from "@/ui/Dialog";
import { Button } from "@/ui/Button";
import { CodeBlock } from "@/ui/Primitives";
import { Input, NativeSelect } from "@/ui/Input";
import { Field } from "@/ui/Primitives";

const SCOPES = ["join", "create", "master", "spectate"] as const;

/** Issues a short-lived grant for a player of this project and shows it once, with the
 * control-plane auth message a client sends first. Lets an operator debug a connection without
 * holding the project's API key. */
export function TestGrantDialog({ open, onOpenChange, projectId, endpoint }: { open: boolean; onOpenChange: (o: boolean) => void; projectId: string; endpoint?: string }) {
  const { t, locale } = useI18n();
  const issue = useIssueTestGrant(projectId);
  const [playerId, setPlayerId] = useState("debug-1");
  const [room, setRoom] = useState("");
  const [scopes, setScopes] = useState<string[]>(["join", "create"]);
  const [ttl, setTtl] = useState(300);
  const [result, setResult] = useState<{ grant: string; expires_at: number } | null>(null);
  const wsUrl = (endpoint ?? "https://<node>").replace(/^http/, "ws") + "/v1/ws";
  if (result) {
    return (
      <Dialog
        open
        onOpenChange={() => {
          setResult(null);
          onOpenChange(false);
        }}
        title={t("grant.title")}
        description={t("grant.expires", { at: fmtDateTimeShort(locale, new Date(result.expires_at * 1000).toISOString()) })}
        footer={
          <Button
            onClick={() => {
              setResult(null);
              onOpenChange(false);
            }}
          >
            {t("common.close")}
          </Button>
        }
      >
        <div data-testid="test-grant">
          <Field label={t("grant.token")}>
            <CodeBlock value={result.grant} />
          </Field>
        </div>
        <Field label={t("grant.snippet")} className="mt-3">
          <CodeBlock value={`${wsUrl}\n${JSON.stringify({ t: "auth", grant: result.grant })}`} />
        </Field>
      </Dialog>
    );
  }
  return (
    <FormDialog
      open={open}
      onOpenChange={onOpenChange}
      title={t("grant.title")}
      description={t("grant.desc")}
      submitLabel={t("grant.issue")}
      disabled={!playerId.trim() || scopes.length === 0}
      onSubmit={async () => {
        const r = await issue.mutateAsync({ player_id: playerId.trim(), name: playerId.trim(), room: room.trim() || undefined, scopes, ttl_secs: ttl });
        setResult(r);
      }}
    >
      <Field label={t("grant.player")} htmlFor="tg-player" required>
        <Input id="tg-player" value={playerId} maxLength={128} onChange={(e) => setPlayerId(e.target.value)} autoFocus />
      </Field>
      <Field label={t("grant.room")} htmlFor="tg-room" hint={t("grant.room.hint")}>
        <Input id="tg-room" value={room} onChange={(e) => setRoom(e.target.value)} placeholder="—" />
      </Field>
      <Field label={t("grant.scopes")}>
        <div className="flex flex-wrap gap-3">
          {SCOPES.map((s) => (
            <Checkbox key={s} label={s} checked={scopes.includes(s)} onChange={(e) => setScopes((cur) => (e.target.checked ? [...cur, s] : cur.filter((x) => x !== s)))} />
          ))}
        </div>
      </Field>
      <Field label={t("grant.ttl")} htmlFor="tg-ttl">
        <NativeSelect id="tg-ttl" value={String(ttl)} onChange={(e) => setTtl(Number(e.target.value))}>
          <option value="300">5 min</option>
          <option value="900">15 min</option>
          <option value="3600">60 min</option>
        </NativeSelect>
      </Field>
    </FormDialog>
  );
}
