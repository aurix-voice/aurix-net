import { useState } from "react";

import type { RotatedWebhook, Webhook } from "@/api/client";
import { useRotateWebhook } from "@/api/hooks";
import { useI18n } from "@/i18n";
import { fmtDateTimeShort } from "@/lib/format";
import { Button } from "@/ui/Button";
import { ConfirmDialog, Dialog } from "@/ui/Dialog";
import { NativeSelect } from "@/ui/Input";
import { Callout, CodeBlock, Field } from "@/ui/Primitives";
import { useToast } from "@/ui/Toast";

const GRACE_HOURS = [1, 24, 72, 168] as const;

/** Confirm → rotate → show the new secret once, with the moment the old one stops signing. */
export function RotateWebhookDialog({ webhook, onClose, projectId }: { webhook: Webhook | null; onClose: () => void; projectId?: string }) {
  const { t, locale } = useI18n();
  const toast = useToast();
  const rotate = useRotateWebhook(projectId);
  const [grace, setGrace] = useState<number>(24);
  const [done, setDone] = useState<RotatedWebhook | null>(null);
  if (done) {
    return (
      <Dialog
        open
        onOpenChange={(o) => {
          if (!o) {
            setDone(null);
            onClose();
          }
        }}
        title={t("webhooks.rotate")}
        footer={
          <Button
            onClick={() => {
              setDone(null);
              onClose();
            }}
          >
            {t("common.close")}
          </Button>
        }
      >
        <Callout tone="warn">{t("webhooks.rotate.shownOnce", { until: fmtDateTimeShort(locale, done.previous_secret_expires_at) })}</Callout>
        <CodeBlock value={done.secret} className="mt-3" />
      </Dialog>
    );
  }
  return (
    <ConfirmDialog
      open={!!webhook}
      onOpenChange={(o) => !o && onClose()}
      title={t("webhooks.rotate")}
      description={t("webhooks.rotate.desc")}
      confirmLabel={t("webhooks.rotate")}
      onConfirm={async () => {
        if (!webhook) return;
        try {
          const r = await rotate.mutateAsync({ webhookId: webhook.id, graceSecs: grace * 3600 });
          setDone(r);
        } catch (err) {
          toast.error(err instanceof Error ? err.message : String(err));
          onClose();
        }
      }}
    >
      <Field label={t("webhooks.rotate.grace")} htmlFor="rotate-grace">
        <NativeSelect id="rotate-grace" value={String(grace)} onChange={(e) => setGrace(Number(e.target.value))}>
          {GRACE_HOURS.map((h) => (
            <option key={h} value={h}>
              {h < 24 ? t("webhooks.rotate.hours", { n: h }) : t("webhooks.rotate.days", { n: h / 24 })}
            </option>
          ))}
        </NativeSelect>
      </Field>
    </ConfirmDialog>
  );
}
