import type { Region } from "@/api/client";
import { useI18n } from "@/i18n";
import { Card, CopyButton, Mono } from "@/ui/Primitives";

/** Everything a game team needs to point a client at this project: region endpoints and a
 * grant call. The API key is never rendered — the snippet reads it from the environment. */
export function ConnectCard({ regions, allowed }: { regions: Region[]; allowed?: string[] }) {
  const { t } = useI18n();
  const shown = allowed?.length ? regions.filter((r) => allowed.includes(r.id)) : regions;
  const first = shown.find((r) => r.endpoints.length)?.endpoints[0] ?? "https://<node>";
  const grant = [
    `curl -s ${first}/v1/grants \\`,
    `  -H "Authorization: Bearer $AURIX_NET_API_KEY" \\`,
    `  -H "Content-Type: application/json" \\`,
    `  -d '{"player_id":"p1","name":"Player","scopes":["join","create"],"ttl_secs":300}'`,
  ].join("\n");
  return (
    <Card className="p-4 space-y-4">
      <div>
        <div className="text-xs font-medium text-fg-muted mb-1.5">{t("projects.connect.endpoints")}</div>
        {shown.length === 0 ? (
          <div className="text-xs text-fg-faint">{t("projects.connect.noRegions")}</div>
        ) : (
          <ul className="space-y-1.5">
            {shown.map((r) => (
              <li key={r.id} className="text-xs">
                <div className="font-medium">{r.display_name || r.id}</div>
                {r.endpoints.map((e) => (
                  <div key={e} className="flex items-center gap-1 min-w-0">
                    <Mono className="truncate text-[11px]">{e}</Mono>
                    <CopyButton value={e} />
                  </div>
                ))}
              </li>
            ))}
          </ul>
        )}
      </div>
      <div>
        <div className="flex items-center justify-between mb-1.5">
          <div className="text-xs font-medium text-fg-muted">{t("projects.connect.grant")}</div>
          <CopyButton value={grant} />
        </div>
        <pre className="text-[11px] leading-relaxed bg-surface-2 border border-border rounded-md p-2.5 overflow-x-auto font-mono whitespace-pre">{grant}</pre>
        <div className="text-[11px] text-fg-faint mt-1.5">{t("projects.connect.hint")}</div>
      </div>
    </Card>
  );
}
