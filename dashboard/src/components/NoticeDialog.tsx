import { useState } from "react";

import { useNotice } from "@/api/hooks";
import { useI18n } from "@/i18n";
import { FormDialog } from "@/ui/Dialog";
import { Input, NativeSelect } from "@/ui/Input";
import { Field } from "@/ui/Primitives";
import { useToast } from "@/ui/Toast";

/** Operator/system message pushed to connected players (a room, or every live room of a project). */
export function NoticeDialog({ open, onOpenChange, roomId, projectId }: { open: boolean; onOpenChange: (o: boolean) => void; roomId?: string; projectId?: string }) {
  const { t } = useI18n();
  const toast = useToast();
  const notice = useNotice();
  const [text, setText] = useState("");
  const [level, setLevel] = useState<"info" | "warn">("info");
  return (
    <FormDialog
      open={open}
      onOpenChange={(o) => {
        onOpenChange(o);
        if (!o) setText("");
      }}
      title={t("notice.title")}
      description={roomId ? t("notice.desc.room") : t("notice.desc.project")}
      submitLabel={t("notice.send")}
      disabled={!text.trim()}
      onSubmit={async () => {
        const r = await notice.mutateAsync({ roomId, projectId, text: text.trim(), level });
        toast.ok(t("notice.sent", { rooms: r.rooms, players: r.players }));
        onOpenChange(false);
        setText("");
      }}
    >
      <Field label={t("notice.text")} htmlFor="notice-text" required>
        <Input id="notice-text" value={text} maxLength={512} onChange={(e) => setText(e.target.value)} autoFocus placeholder={t("notice.placeholder")} />
      </Field>
      <Field label={t("notice.level")} htmlFor="notice-level">
        <NativeSelect id="notice-level" value={level} onChange={(e) => setLevel(e.target.value as "info" | "warn")}>
          <option value="info">{t("notice.level.info")}</option>
          <option value="warn">{t("notice.level.warn")}</option>
        </NativeSelect>
      </Field>
    </FormDialog>
  );
}
