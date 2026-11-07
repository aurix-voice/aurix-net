import { Boxes, Lock } from "lucide-react";
import type { ReactNode } from "react";

import { useAuth } from "@/auth/AuthProvider";
import { useI18n } from "@/i18n";
import { EmptyState } from "@/ui/Primitives";

export function Forbidden({ scope }: { scope?: string }) {
  const { t } = useI18n();
  return (
    <EmptyState
      icon={<Lock className="size-5" />}
      title={t("common.forbidden")}
      description={
        <>
          {t("common.forbidden.desc")}
          {scope ? <div className="mt-1 font-mono text-[11px]">{t("common.forbidden.scope", { scope })}</div> : null}
        </>
      }
    />
  );
}

/** Operator-only pages (projects, fleet). */
export function RequireOperator({ children }: { children: ReactNode }) {
  const { operator } = useAuth();
  if (!operator) return <Forbidden scope="operator" />;
  return <>{children}</>;
}

/** API-key scope gate for project pages. */
export function RequireScope({ scope, projectKey, children }: { scope: "rooms" | "read" | "admin"; projectKey?: boolean; children: ReactNode }) {
  const { t } = useI18n();
  const { can, operator } = useAuth();
  if (projectKey && operator) {
    return <EmptyState icon={<Lock className="size-5" />} title={t("common.forbidden")} description={t("common.forbidden.operator")} />;
  }
  if (!can(scope)) return <Forbidden scope={scope} />;
  return <>{children}</>;
}

/** Project pages need a project in scope; the operator picks one in the top bar. */
export function RequireProject({ children }: { children: (projectId: string) => ReactNode }) {
  const { t } = useI18n();
  const { projectId } = useAuth();
  if (!projectId) {
    return <EmptyState icon={<Boxes className="size-5" />} title={t("common.selectProject")} description={t("common.selectProject.desc")} />;
  }
  return <>{children(projectId)}</>;
}
