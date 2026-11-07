import { useRef, useState } from "react";

import type { NewKey } from "@/api/client";
import { useI18n } from "@/i18n";
import { Button } from "@/ui/Button";
import { Dialog, FormDialog } from "@/ui/Dialog";
import { Checkbox, Input } from "@/ui/Input";
import { Callout, CodeBlock, Field } from "@/ui/Primitives";

const SCOPES = ["rooms", "read", "admin"] as const;

/** Create form + one-time plaintext reveal; the key is never fetchable again. */
export function NewKeyDialog({ open, onOpenChange, create }: { open: boolean; onOpenChange: (o: boolean) => void; create: (body: { name: string; scopes: string[] }) => Promise<NewKey> }) {
  const { t } = useI18n();
  const [name, setName] = useState("");
  const [scopes, setScopes] = useState<string[]>(["rooms", "read"]);
  const [created, setCreated] = useState<NewKey | null>(null);
  const keepOpen = useRef(false);
  const toggle = (s: string) => setScopes((cur) => (cur.includes(s) ? cur.filter((x) => x !== s) : [...cur, s]));
  const close = (o: boolean) => {
    if (!o && keepOpen.current) {
      keepOpen.current = false;
      return;
    }
    if (!o) {
      setCreated(null);
      setName("");
    }
    onOpenChange(o);
  };
  if (created) {
    return (
      <Dialog open={open} onOpenChange={close} title={t("keys.new")} description={t("keys.created")} footer={<Button onClick={() => close(false)}>{t("common.close")}</Button>}>
        <Callout tone="warn">{t("keys.created")}</Callout>
        <CodeBlock value={created.key} className="mt-3" />
      </Dialog>
    );
  }
  return (
    <FormDialog
      open={open}
      onOpenChange={close}
      title={t("keys.new")}
      submitLabel={t("common.create")}
      disabled={!name.trim() || scopes.length === 0}
      onSubmit={async () => {
        const k = await create({ name: name.trim(), scopes });
        keepOpen.current = true;
        setCreated(k);
      }}
    >
      <Field label={t("common.name")} htmlFor="key-name" required>
        <Input id="key-name" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
      </Field>
      <Field label={t("keys.scopes")}>
        <div className="flex flex-col gap-1.5">
          {SCOPES.map((s) => (
            <Checkbox key={s} label={t(`keys.scope.${s}`)} checked={scopes.includes(s)} onChange={() => toggle(s)} />
          ))}
        </div>
      </Field>
    </FormDialog>
  );
}
