import { useI18n } from "@/i18n";
import { Dialog } from "@/ui/Dialog";
import { CodeBlock, Mono } from "@/ui/Primitives";

/** Static reference for game teams wiring a receiver: envelope, events with sample `data`,
 * and how to verify `X-Aurix-Signature`. Mirrors docs/webhooks.md. */
export const WEBHOOK_EVENTS = [
  { name: "room.created", data: `{"room":"<uuid>","name":"lobby-1","mode":"relay|sync|lockstep","region":"eu"}`, when: "a room was opened (by a client join with create, or by POST /v1/rooms)" },
  { name: "room.closed", data: `{"room":"<uuid>","name":"ranked-7","mode":"sync","props":{},"state":"<base64, with room prop export_state_on_close>"}`, when: "the room ended: last player left and empty_ttl passed, or it was closed by the game / an operator" },
  { name: "room.rehosted", data: `{"room":"<uuid>","name":"lobby-1","mode":"sync","node":"<node-uuid>","restored":true}`, when: "the room was adopted by another node after a drain or node loss; restored is false when no checkpoint was found and the room started fresh" },
  { name: "player.joined", data: `{"room":"<uuid>","actor":3}`, when: "a player finished auth + join; actor is the per-room id (0..N)" },
  { name: "player.left", data: `{"room":"<uuid>","actor":3,"kicked":true}`, when: "a player left or timed out (player_ttl); kicked is present only for kicks" },
  { name: "join.validate", data: `{"room":"<uuid>","name":"ranked-7","mode":"sync","room_props":{},"player_id":"p1","player_name":"Ann","player_props":{},"spectator":false}`, when: "synchronous pre-join check: answer 2xx with {\"allow\":true} within 2 s, or {\"allow\":false,\"reason\":\"…\"} to refuse; receiver down = join refused. Subscribe explicitly (`*` does not include it)" },
  { name: "match.found", data: `{"room":"lobby-7f3a","mode":"sync","players":["p1","p2"],"tickets":["<uuid>","<uuid>"]}`, when: "matchmaking grouped tickets; `room` is the room name the players will join with their grants (`tickets` lists the matched ticket ids)" },
  { name: "test", data: `{"webhook":"<uuid>"}`, when: "sent by POST /v1/webhooks/:id/test or the console; ignores the event filter" },
] as const;

const ENVELOPE = `{"event":"player.joined","project":"<project-uuid>","at":"2026-11-07T16:20:00Z","data":{"room":"<uuid>","actor":3}}`;
const VERIFY = `import { createHmac, timingSafeEqual } from "node:crypto";
const expected = "sha256=" + createHmac("sha256", SECRET).update(rawBody).digest("hex");
const ok = sig.length === expected.length && timingSafeEqual(Buffer.from(sig), Buffer.from(expected));`;

export function WebhookEventCatalog({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const { t } = useI18n();
  return (
    <Dialog open={open} onOpenChange={onOpenChange} title={t("webhooks.catalog")} description={t("webhooks.catalog.desc")} size="lg">
      <div className="space-y-4 text-sm">
        <div>
          <div className="text-xs font-medium text-fg-muted mb-1.5">{t("webhooks.catalog.envelope")}</div>
          <CodeBlock value={ENVELOPE} />
        </div>
        <table className="w-full text-[13px] border-collapse">
          <thead>
            <tr className="border-b border-border text-left text-xs text-fg-muted">
              <th className="py-1.5 pr-3 font-medium">{t("webhooks.catalog.event")}</th>
              <th className="py-1.5 pr-3 font-medium">data</th>
              <th className="py-1.5 font-medium">{t("webhooks.catalog.when")}</th>
            </tr>
          </thead>
          <tbody>
            {WEBHOOK_EVENTS.map((e) => (
              <tr key={e.name} className="border-b border-border last:border-0 align-top">
                <td className="py-1.5 pr-3"><Mono>{e.name}</Mono></td>
                <td className="py-1.5 pr-3"><Mono className="text-[11px] break-all">{e.data}</Mono></td>
                <td className="py-1.5 text-fg-muted">{e.when}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <div>
          <div className="text-xs font-medium text-fg-muted mb-1.5">{t("webhooks.catalog.verify")}</div>
          <CodeBlock value={VERIFY} />
          <p className="text-xs text-fg-muted mt-1.5">{t("webhooks.catalog.delivery")}</p>
        </div>
      </div>
    </Dialog>
  );
}
