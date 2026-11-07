import { Link } from "@tanstack/react-router";
import { Activity, Ban, BarChart3, Boxes, Cpu, DoorOpen, KeyRound, Settings, LayoutDashboard, ScrollText, Server, Shuffle, Webhook } from "lucide-react";
import type { ReactNode } from "react";

import { useAuth } from "@/auth/AuthProvider";
import { useI18n, type MessageKey } from "@/i18n";
import { cn } from "@/lib/cn";

import { Logo } from "./Logo";

type To = "/" | "/nodes" | "/projects" | "/rooms" | "/usage" | "/keys" | "/webhooks" | "/bans" | "/modules" | "/matchmaking" | "/audit" | "/settings";
interface NavItem {
  to: To;
  label: MessageKey;
  icon: ReactNode;
  operator?: boolean;
  scope?: "rooms" | "read" | "admin";
  projectKey?: boolean;
  exact?: boolean;
}

const SECTIONS: Array<{ label: MessageKey; items: NavItem[] }> = [
  {
    label: "nav.section.fleet",
    items: [
      { to: "/", label: "nav.overview", icon: <LayoutDashboard className="size-4" />, exact: true },
      { to: "/nodes", label: "nav.nodes", icon: <Server className="size-4" /> },
      { to: "/projects", label: "nav.projects", icon: <Boxes className="size-4" />, operator: true },
    ],
  },
  {
    label: "nav.section.project",
    items: [
      { to: "/rooms", label: "nav.rooms", icon: <DoorOpen className="size-4" /> },
      { to: "/usage", label: "nav.usage", icon: <BarChart3 className="size-4" />, scope: "read" },
      { to: "/keys", projectKey: true, label: "nav.keys", icon: <KeyRound className="size-4" />, scope: "admin" },
      { to: "/webhooks", projectKey: true, label: "nav.webhooks", icon: <Webhook className="size-4" />, scope: "admin" },
      { to: "/modules", projectKey: true, label: "nav.modules", icon: <Cpu className="size-4" />, scope: "admin" },
      { to: "/bans", projectKey: true, label: "nav.bans", icon: <Ban className="size-4" />, scope: "rooms" },
      { to: "/matchmaking", projectKey: true, label: "nav.matchmaking", icon: <Shuffle className="size-4" />, scope: "rooms" },
    ],
  },
  {
    label: "nav.section.system",
    items: [
      { to: "/audit", label: "nav.audit", icon: <ScrollText className="size-4" />, scope: "read" },
      { to: "/settings", operator: true, label: "nav.settings", icon: <Settings className="size-4" /> },
    ],
  },
];

export function Sidebar({ className, onNavigate }: { className?: string; onNavigate?: () => void }) {
  const { t } = useI18n();
  const { operator, can } = useAuth();
  const visible = (i: NavItem) => (i.operator ? operator : true) && (i.projectKey ? !operator : true) && (i.scope ? operator || can(i.scope) : true);
  return (
    <aside className={cn("w-60 shrink-0 border-r border-border bg-surface flex-col", className)}>
      <div className="h-14 flex items-center gap-2.5 px-5 border-b border-border">
        <Logo className="size-6" />
        <span className="text-[13px] font-semibold tracking-tight">{t("app.name")}</span>
        <span className="text-[13px] text-fg-faint">{t("app.tagline")}</span>
      </div>
      <nav className="flex-1 overflow-y-auto px-3 py-4 space-y-5">
        {SECTIONS.map((s) => {
          const items = s.items.filter(visible);
          if (items.length === 0) return null;
          return (
            <div key={s.label}>
              <div className="px-2 mb-1.5 text-[11px] uppercase tracking-wide text-fg-faint">{t(s.label)}</div>
              <ul className="space-y-0.5">
                {items.map((i) => (
                  <li key={i.to}>
                    <Link
                      to={i.to}
                      onClick={onNavigate}
                      activeOptions={{ exact: i.exact }}
                      className="flex items-center gap-2.5 px-2 h-8 rounded-md text-[13px] text-fg-muted hover:text-fg hover:bg-surface-2 [&.active]:bg-surface-2 [&.active]:text-fg [&.active]:font-medium"
                    >
                      {i.icon}
                      {t(i.label)}
                    </Link>
                  </li>
                ))}
              </ul>
            </div>
          );
        })}
      </nav>
      <div className="px-5 py-3 border-t border-border flex items-center justify-between text-[11px] text-fg-faint">
        <span className="flex items-center gap-1.5">
          <Activity className="size-3" /> {__DASHBOARD_VERSION__}
        </span>
        <a href={__DASHBOARD_SOURCE_URL__} target="_blank" rel="noreferrer" className="hover:text-fg">
          {t("shell.source")}
        </a>
      </div>
    </aside>
  );
}
