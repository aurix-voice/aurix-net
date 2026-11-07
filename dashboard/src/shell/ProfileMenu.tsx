import { Languages, LogOut, Monitor, Moon, Sun, KeyRound, ShieldCheck } from "lucide-react";

import { useAuth } from "@/auth/AuthProvider";
import { useI18n } from "@/i18n";
import { useTheme, type Theme } from "@/theme";
import { Menu, type MenuItem } from "@/ui/Menu";

export function ProfileMenu() {
  const { t, locale, setLocale } = useI18n();
  const { me, operator, signOut } = useAuth();
  const { theme, setTheme } = useTheme();
  if (!me) return null;
  const themeItem = (value: Theme, icon: MenuItem["icon"], label: string): MenuItem => ({ label, icon, checked: theme === value, onSelect: () => setTheme(value) });
  const items: MenuItem[] = [
    {
      label: (
        <div className="min-w-0">
          <div className="font-medium truncate">{operator ? t("nav.operator") : me.project?.name}</div>
          <div className="text-[11px] text-fg-muted truncate">{operator ? me.region : `${t("nav.project")} · ${me.key?.name} · ${me.key?.scopes.join(", ")}`}</div>
        </div>
      ),
      disabled: true,
    },
    { label: `${t("nav.language")}: ${locale === "ru" ? "Русский" : "English"}`, icon: <Languages className="size-3.5" />, onSelect: () => setLocale(locale === "ru" ? "en" : "ru"), separatorBefore: true },
    { ...themeItem("light", <Sun className="size-3.5" />, t("nav.theme.light")), separatorBefore: true },
    themeItem("dark", <Moon className="size-3.5" />, t("nav.theme.dark")),
    themeItem("system", <Monitor className="size-3.5" />, t("nav.theme.system")),
    { label: t("nav.signOut"), icon: <LogOut className="size-3.5" />, separatorBefore: true, danger: true, onSelect: () => signOut("manual") },
  ];
  return (
    <Menu
      label={t("nav.profile")}
      items={items}
      trigger={
        <button type="button" className="size-8 rounded-full bg-surface-2 border border-border grid place-items-center text-fg-muted hover:text-fg" aria-label={t("nav.profile")}>
          {operator ? <ShieldCheck className="size-4" /> : <KeyRound className="size-4" />}
        </button>
      }
    />
  );
}
