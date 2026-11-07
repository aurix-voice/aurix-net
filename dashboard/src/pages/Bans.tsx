import { BansPanel } from "@/components/BansPanel";
import { PlayerLookup } from "@/components/PlayerLookup";
import { useI18n } from "@/i18n";
import { RequireScope } from "@/shell/Guards";
import { PageHeader } from "@/ui/Page";
import { Card } from "@/ui/Primitives";

export function BansPage() {
  const { t } = useI18n();
  return (
    <RequireScope scope="rooms" projectKey>
      <PageHeader title={t("bans.title")} description={t("bans.desc")} />
      <Card className="mb-5">
        <PlayerLookup />
      </Card>
      <Card>
        <BansPanel />
      </Card>
    </RequireScope>
  );
}
