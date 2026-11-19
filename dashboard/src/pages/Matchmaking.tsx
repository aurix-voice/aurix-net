import { useState } from "react";

import { useCancelTicket, useFileTicket, useTicket } from "@/api/hooks";
import { MatchmakingQueue } from "@/components/MatchmakingQueue";
import { useI18n } from "@/i18n";
import { RequireScope } from "@/shell/Guards";
import { Button } from "@/ui/Button";
import { Input, NativeSelect } from "@/ui/Input";
import { PageHeader } from "@/ui/Page";
import { Badge, Card, CardHeader, Field, IdChip, KV } from "@/ui/Primitives";
import { useToast } from "@/ui/Toast";

function MatchmakingBody() {
  const { t } = useI18n();
  const toast = useToast();
  const file = useFileTicket();
  const cancel = useCancelTicket();
  const [mode, setMode] = useState("relay");
  const [size, setSize] = useState(2);
  const [player, setPlayer] = useState("console-tester");
  const [ticketId, setTicketId] = useState<string | null>(null);
  const ticket = useTicket(ticketId);
  const state = ticket.data?.state.state ?? (file.isPending ? "…" : null);
  const room = ticket.data?.state.state === "matched" ? ticket.data.state.room : null;
  return (
    <>
      <PageHeader title={t("mm.title")} description={t("mm.desc")} />
      <div className="grid gap-4 lg:grid-cols-2">
        <Card className="p-4">
          <CardHeader title={t("mm.file")} className="px-0 pt-0" />
          <form
            className="space-y-3"
            onSubmit={(e) => {
              e.preventDefault();
              file.mutate({ mode, max_players: size, player_id: player, props: {} }, { onSuccess: (r) => setTicketId(r.ticket), onError: (err) => toast.error(err.message) });
            }}
          >
            <Field label={t("mm.mode")} htmlFor="mm-mode">
              <NativeSelect id="mm-mode" value={mode} onChange={(e) => setMode(e.target.value)}>
                <option value="relay">relay</option>
                <option value="sync">sync</option>
                <option value="lockstep">lockstep</option>
              </NativeSelect>
            </Field>
            <Field label={t("mm.size")} htmlFor="mm-size">
              <Input id="mm-size" type="number" min={1} max={128} value={size} onChange={(e) => setSize(Number(e.target.value))} />
            </Field>
            <Field label={t("rooms.playerId")} htmlFor="mm-player">
              <Input id="mm-player" value={player} onChange={(e) => setPlayer(e.target.value)} />
            </Field>
            <Button type="submit" loading={file.isPending}>
              {t("mm.file")}
            </Button>
          </form>
        </Card>
        <Card className="p-4">
          <CardHeader title={t("mm.ticket")} className="px-0 pt-0" />
          {ticketId ? (
            <>
              <KV
                cols={1}
                items={[
                  { k: t("mm.ticket"), v: <IdChip id={ticketId} short={36} /> },
                  { k: t("mm.state"), v: state ? <Badge tone={state === "matched" ? "ok" : state === "cancelled" ? "neutral" : "accent"} dot>{state}</Badge> : "—" },
                  { k: t("rooms.title"), v: room ? <IdChip id={room} /> : "—", hidden: !room },
                ]}
              />
              <Button
                variant="secondary"
                size="sm"
                className="mt-4"
                onClick={() => cancel.mutate(ticketId, { onSuccess: () => void ticket.refetch(), onError: (err) => toast.error(err.message) })}
                disabled={state === "matched" || state === "cancelled"}
              >
                {t("mm.cancel")}
              </Button>
            </>
          ) : (
            <p className="text-[13px] text-fg-muted">{t("common.empty")}</p>
          )}
        </Card>
      </div>
    </>
  );
}

export function MatchmakingPage() {
  const { t } = useI18n();
  return (
    <RequireScope scope="rooms" projectKey>
      <MatchmakingBody />
      <Card className="mt-5">
        <CardHeader title={t("mm.queue.title")} />
        <MatchmakingQueue />
      </Card>
    </RequireScope>
  );
}
