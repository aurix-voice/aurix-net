import type { Webhook } from "@/api/client";
import { useTestWebhook, useWebhookDeliveries } from "@/api/hooks";
import { useI18n } from "@/i18n";
import { fmtDateTimeShort } from "@/lib/format";
import { Button } from "@/ui/Button";
import { Dialog } from "@/ui/Dialog";
import { Badge, Mono } from "@/ui/Primitives";
import { DataTable } from "@/ui/Table";
import { useToast } from "@/ui/Toast";

/** Recent deliveries of one webhook with a "send test" action. Operator sessions pass the
 * project id so the operator routes are used. */
export function WebhookDeliveriesDialog({ webhook, projectId, fleet, onClose }: { webhook: Pick<Webhook, "id" | "url"> | null; projectId?: string; fleet?: boolean; onClose: () => void }) {
  const { t, locale } = useI18n();
  const toast = useToast();
  const q = useWebhookDeliveries(webhook?.id ?? null, projectId, fleet);
  const test = useTestWebhook(projectId, fleet);
  return (
    <Dialog
      open={!!webhook}
      onOpenChange={(o) => !o && onClose()}
      title={t("webhooks.deliveries")}
      description={webhook ? <Mono className="break-all">{webhook.url}</Mono> : undefined}
      size="lg"
      footer={
        <>
          <Button
            variant="secondary"
            disabled={!webhook || test.isPending}
            onClick={() => {
              if (!webhook) return;
              test.mutate(webhook.id, {
                onSuccess: () => toast.ok(t("webhooks.test.queued")),
                onError: (e) => toast.error(t("webhooks.test"), e instanceof Error ? e.message : String(e)),
              });
            }}
          >
            {t("webhooks.test")}
          </Button>
          <Button onClick={onClose}>{t("common.close")}</Button>
        </>
      }
    >
      <DataTable
        rows={q.data}
        rowKey={(d) => d.id}
        dense
        loading={q.isLoading}
        error={q.error}
        empty={<span className="block px-3 py-6 text-center text-sm text-fg-muted">{t("webhooks.deliveries.empty")}</span>}
        columns={[
          { key: "when", header: t("audit.when"), cell: (d) => <span className="text-fg-muted whitespace-nowrap">{fmtDateTimeShort(locale, d.created_at)}</span> },
          { key: "event", header: t("webhooks.events"), cell: (d) => <Mono>{d.event}</Mono> },
          {
            key: "result",
            header: t("webhooks.result"),
            cell: (d) => (
              <span className="flex items-center gap-1.5">
                <Badge tone={d.ok ? "ok" : "danger"} dot>
                  {d.status ?? (d.ok ? "OK" : t("webhooks.failed"))}
                </Badge>
                {d.error ? <span className="text-xs text-fg-muted truncate max-w-[280px]" title={d.error}>{d.error}</span> : null}
              </span>
            ),
          },
          { key: "attempts", header: t("webhooks.attempts"), align: "right", cell: (d) => d.attempts },
          { key: "latency", header: t("webhooks.latency"), align: "right", cell: (d) => `${d.latency_ms} ms` },
        ]}
      />
    </Dialog>
  );
}
