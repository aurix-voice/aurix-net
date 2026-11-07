import { Boxes, ChevronDown } from "lucide-react";

import { useProjects } from "@/api/hooks";
import { useAuth } from "@/auth/AuthProvider";
import { useI18n } from "@/i18n";
import { Button } from "@/ui/Button";
import { Menu } from "@/ui/Menu";

/** Operator sessions pick the project that project pages act on; key sessions are pinned. */
export function ProjectSwitcher() {
  const { t } = useI18n();
  const { operator, me, projectId, selectProject } = useAuth();
  const projects = useProjects();
  if (!operator) {
    return (
      <div className="flex items-center gap-2 text-[13px] min-w-0">
        <Boxes className="size-4 text-fg-muted shrink-0" />
        <span className="font-medium truncate">{me?.project?.name}</span>
        <span className="text-fg-faint hidden sm:inline">· {me?.key?.name}</span>
      </div>
    );
  }
  const current = projects.data?.find((p) => p.id === projectId);
  const items = [
    { label: t("scope.all"), onSelect: () => selectProject(null) },
    ...(projects.data ?? []).map((p) => ({ label: p.name, onSelect: () => selectProject(p.id) })),
  ];
  return (
    <Menu
      label={t("scope.pick")}
      items={items}
      trigger={
        <Button variant="ghost" size="sm" className="max-w-[260px]">
          <Boxes className="size-4 text-fg-muted" />
          <span className="truncate">{current?.name ?? t("scope.all")}</span>
          <ChevronDown className="size-3.5 text-fg-faint" />
        </Button>
      }
    />
  );
}
