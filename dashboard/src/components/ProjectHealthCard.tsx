import { useState } from "react";

import type { ProjectDetail, Rejections } from "@/api/client";
import { type MessageKey, useI18n } from "@/i18n";
import { Stat } from "@/ui/Primitives";

const WEEK_MS = 7 * 24 * 3600 * 1000;

/** One-glance state of a project: suspension, live load vs the room quota, rejections,
 * webhook failures and keys about to expire. Everything comes from the detail payload plus the
 * rejections query, so there is no extra round trip. */
export function ProjectHealthCard({ detail, rejections }: { detail: ProjectDetail; rejections?: Rejections }) {
  const { t } = useI18n();
  const [now] = useState(() => Date.now());
  const p = detail.project;
  const liveRooms = detail.rooms.filter((r) => r.open).length;
  const players = detail.rooms.reduce((a, r) => a + r.players, 0);
  const maxRooms = typeof detail.effective_limits.max_rooms === "number" ? detail.effective_limits.max_rooms : undefined;
  const rejectedTotal = rejections ? Object.values(rejections.total).reduce((a, b) => a + b, 0) : 0;
  const top = rejections ? Object.entries(rejections.total).sort((a, b) => b[1] - a[1])[0] : undefined;
  const openKeys = detail.keys.filter((k) => !k.revoked_at);
  const expiring = openKeys.filter((k) => k.expires_at && new Date(k.expires_at).getTime() - now < WEEK_MS).length;
  const wh = detail.webhooks_1h;
  const roomsTone = maxRooms !== undefined && liveRooms >= maxRooms ? "warn" : "neutral";
  const capMinutes = typeof detail.effective_limits.monthly_player_minutes === "number" ? detail.effective_limits.monthly_player_minutes : undefined;
  const capBytes = typeof detail.effective_limits.monthly_bytes === "number" ? detail.effective_limits.monthly_bytes : undefined;
  const usedMinutes = Math.round(detail.usage_month.player_minutes);
  const usedMb = Math.round(detail.usage_month.bytes / 1e6);
  const pct = (used: number, cap?: number) => (cap ? used / cap : 0);
  const capPct = Math.max(pct(detail.usage_month.player_minutes, capMinutes), pct(detail.usage_month.bytes, capBytes));
  const usageTone = capPct >= 1 ? "danger" : capPct >= 0.8 ? "warn" : "neutral";
  return (
    <div data-testid="project-health" className="grid gap-3 grid-cols-2 md:grid-cols-3 xl:grid-cols-6">
      <Stat label={t("health.status")} value={p.disabled_at ? t("health.suspended") : t("health.active")} tone={p.disabled_at ? "danger" : "ok"} />
      <Stat label={t("health.rooms")} value={maxRooms !== undefined ? `${liveRooms} / ${maxRooms}` : String(liveRooms)} hint={t("health.players", { n: players })} tone={roomsTone} />
      <Stat
        label={t("health.rejections")}
        value={String(rejectedTotal)}
        hint={top && top[1] > 0 ? t("health.top", { reason: t(`rejections.${top[0]}` as MessageKey) }) : t("health.none")}
        tone={rejectedTotal > 0 ? "warn" : "neutral"}
      />
      <Stat
        label={t("health.webhooks")}
        value={detail.webhooks.length ? `${wh.failed} / ${wh.total}` : "—"}
        hint={detail.webhooks.length ? t("health.webhooks.hint") : t("health.webhooks.none")}
        tone={wh.failed > 0 ? "danger" : "neutral"}
      />
      <Stat
        label={t("health.usage")}
        value={capMinutes ? `${usedMinutes.toLocaleString()} / ${capMinutes.toLocaleString()}` : usedMinutes.toLocaleString()}
        hint={capBytes ? t("health.usage.bytes.cap", { mb: usedMb.toLocaleString(), cap: Math.round(capBytes / 1e6).toLocaleString() }) : t("health.usage.bytes", { mb: usedMb.toLocaleString() })}
        tone={usageTone}
      />
      <Stat label={t("health.keys")} value={String(openKeys.length)} hint={expiring ? t("health.keys.expiring", { n: expiring }) : t("health.none")} tone={expiring ? "warn" : "neutral"} />
    </div>
  );
}
