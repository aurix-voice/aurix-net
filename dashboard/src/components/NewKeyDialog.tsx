import { useRef, useState } from "react";

import type { NewKey } from "@/api/client";
import { useI18n } from "@/i18n";
import { Button } from "@/ui/Button";
import { Dialog, FormDialog } from "@/ui/Dialog";
import { Checkbox, Input, NativeSelect } from "@/ui/Input";
import { Callout, CodeBlock, Field } from "@/ui/Primitives";

const SCOPES = ["rooms", "read", "admin"] as const;
const EXPIRIES = [0, 30, 90, 365] as const;

/** Create form + one-time plaintext reveal; the key is never fetchable again. */
export function NewKeyDialog({ open, onOpenChange, create }: { open: boolean; onOpenChange: (o: boolean) => void; create: (body: { name: string; scopes: string[]; expires_in_secs?: number }) => Promise<NewKey> }) {
  const { t } = useI18n();
  const [name, setName] = useState("");
  const [scopes, setScopes] = useState<string[]>(["rooms", "read"]);
  const [expiryDays, setExpiryDays] = useState<number>(0);
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
        const k = await create({ name: name.trim(), scopes, expires_in_secs: expiryDays ? expiryDays * 86_400 : undefined });
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
      <Field label={t("keys.expires")} htmlFor="key-expiry">
        <NativeSelect id="key-expiry" value={String(expiryDays)} onChange={(e) => setExpiryDays(Number(e.target.value))}>
          {EXPIRIES.map((d) => (
            <option key={d} value={d}>
              {d === 0 ? t("keys.expires.never") : t("keys.expires.days", { d })}
            </option>
          ))}
        </NativeSelect>
      </Field>
    </FormDialog>
  );
}
