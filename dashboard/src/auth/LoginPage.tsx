import { useNavigate, useSearch } from "@tanstack/react-router";
import { useState, type FormEvent } from "react";

import { errorKind, errorMessage } from "@/api/client";
import { useAuth } from "@/auth/AuthProvider";
import { useI18n } from "@/i18n";
import { Button } from "@/ui/Button";
import { Input } from "@/ui/Input";
import { Callout, Card, Field } from "@/ui/Primitives";

export function LoginPage() {
  const { t, locale } = useI18n();
  const { signIn, lastSignOutReason } = useAuth();
  const navigate = useNavigate();
  const search = useSearch({ from: "/auth/login" });
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await signIn(token.trim());
      void navigate({ to: search.redirect ?? "/", replace: true });
    } catch (err) {
      setError(errorKind(err) === "unauthorized" ? t("auth.invalid") : errorMessage(err, locale));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card className="p-6">
      <h1 className="text-lg font-semibold tracking-tight">{t("auth.title")}</h1>
      <p className="text-[13px] text-fg-muted mt-1">{t("auth.subtitle")}</p>
      {lastSignOutReason === "expired" ? (
        <Callout tone="warn" className="mt-4">
          {t("auth.sessionExpired")}
        </Callout>
      ) : null}
      <form onSubmit={onSubmit} className="mt-5 space-y-3" noValidate>
        <Field label={t("auth.token")} htmlFor="login-token" hint={t("auth.token.hint")}>
          <Input id="login-token" type="password" name="token" autoComplete="off" required autoFocus value={token} onChange={(e) => setToken(e.target.value)} className="font-mono" />
        </Field>
        {error ? <Callout tone="danger">{error}</Callout> : null}
        <Button type="submit" className="w-full" loading={busy} disabled={!token.trim()}>
          {busy ? t("auth.signingIn") : t("auth.signIn")}
        </Button>
      </form>
      <p className="mt-5 pt-4 border-t border-border text-[12px] text-fg-muted">{t("auth.note")}</p>
    </Card>
  );
}
