import type { AgentInfo, RateLimit, ThreadUsage } from "~/api/types";
import { type Translate, useT } from "~/i18n";
import { cn } from "~/lib/utils";
import { type UsageDisplay, usePrefs } from "~/state/prefs";
import { useNow } from "../hooks";
import { Tip } from "../ui/overlay";

export interface RateLimitSource {
  limits: RateLimit[];
  /** Set when the limits come from the agent's stored snapshot rather than the live thread. */
  updatedAt: number | null;
  provider: string | null;
}

/** Live turn data wins; otherwise the snapshot the core kept for the agent. */
export function rateLimitSource(usage: ThreadUsage | null | undefined, agent: AgentInfo | null | undefined): RateLimitSource {
  const provider = agent?.auth.provider ?? null;
  if (usage?.rateLimits && usage.rateLimits.length > 0) return { limits: usage.rateLimits, updatedAt: null, provider };
  return { limits: agent?.rateLimits ?? [], updatedAt: agent?.rateLimitsUpdatedAt ?? null, provider };
}

const clampPercent = (value: number) => Math.max(0, Math.min(100, Math.round(value)));

export const remainingPercent = (limit: RateLimit) => clampPercent(100 - limit.usedPercent);
export const usedPercent = (limit: RateLimit) => clampPercent(limit.usedPercent);

export function shownPercent(limit: RateLimit, display: UsageDisplay) {
  return display === "used" ? usedPercent(limit) : remainingPercent(limit);
}

export function limitTone(limit: RateLimit) {
  if (limit.usedPercent >= 100) return "text-destructive";
  if (limit.usedPercent >= 80) return "text-warning";
  return "text-muted-foreground";
}

export function limitBarTone(limit: RateLimit) {
  if (limit.usedPercent >= 100) return "bg-destructive";
  if (limit.usedPercent >= 80) return "bg-warning";
  return "bg-muted-foreground/60";
}

export function formatSpan(ms: number, t: Translate) {
  const minutes = Math.max(1, Math.round(ms / 60_000));
  if (minutes < 60) return t("time.minutes", { n: minutes });
  const hours = Math.round(minutes / 60);
  if (hours < 24) return t("time.hours", { n: hours });
  return t("time.days", { n: Math.round(hours / 24) });
}

export function resetLabel(limit: RateLimit, now: number, t: Translate) {
  if (limit.resetsAt == null) return null;
  if (limit.resetsAt <= now) return t("usage.reset");
  return t("usage.resetsIn", { time: formatSpan(limit.resetsAt - now, t) });
}

export function snapshotAge(updatedAt: number | null, now: number, t: Translate) {
  if (updatedAt == null) return null;
  const age = now - updatedAt;
  return age < 60_000 ? t("usage.updatedNow") : t("usage.updated", { time: formatSpan(age, t) });
}

/** "codex (5h)", "5-hour" or "weekly" become "5h" / "7d" for the inline pill; tooltips keep the agent's label. */
export function shortLimitLabel(label: string) {
  const inner = (/^[a-z0-9_-]+\s*\(([^)]+)\)$/i.exec(label.trim())?.[1] ?? label).trim();
  const lower = inner.toLowerCase();
  if (lower === "weekly") return "7d";
  if (lower === "daily") return "1d";
  const span = /^(\d+)\s*-?\s*(m|mins?|minutes?|h|hrs?|hours?|d|days?|w|wks?|weeks?)$/.exec(lower);
  return span ? `${span[1]}${span[2]![0]}` : inner;
}

/** The two most constrained windows, kept in the order the agent reported them. */
function headline(limits: RateLimit[]) {
  const top = [...limits].sort((a, b) => b.usedPercent - a.usedPercent).slice(0, 2);
  return limits.filter((limit) => top.includes(limit));
}

export function RateLimitBadge({ source }: { source: RateLimitSource }) {
  const t = useT();
  const display = usePrefs((s) => s.usageDisplay);
  const now = useNow(30_000);
  const shown = headline(source.limits);
  const worst = shown.reduce((a, b) => (b.usedPercent > a.usedPercent ? b : a));
  const age = snapshotAge(source.updatedAt, now, t);
  const label = (
    <div className="flex flex-col gap-1">
      {source.limits.map((limit) => {
        const reset = resetLabel(limit, now, t);
        return (
          <div key={limit.label} className="flex items-baseline justify-between gap-4 tabular-nums">
            <span className="font-medium">{limit.label}</span>
            <span className="text-muted-foreground">
              {t("usage.remaining", { n: remainingPercent(limit) })} · {t("usage.used", { n: usedPercent(limit) })}
              {reset ? ` · ${reset}` : ""}
            </span>
          </div>
        );
      })}
      {age ? <span className="text-muted-foreground">{age}</span> : null}
      {source.provider ? <span className="text-muted-foreground/80">{t("usage.routed", { provider: source.provider })}</span> : null}
    </div>
  );
  return (
    <Tip label={label}>
      <span
        aria-label={t("usage.limits")}
        className={cn("inline-flex h-6 shrink-0 items-center gap-1 rounded-md px-1.5 text-xs tabular-nums", limitTone(worst))}
      >
        {shown.map((limit, index) => (
          <span key={limit.label} className={cn("whitespace-nowrap", limitTone(limit))}>
            {index > 0 ? <span className="me-1 text-muted-foreground/60">·</span> : null}
            <span className="opacity-75">{shortLimitLabel(limit.label)}</span> {shownPercent(limit, display)}%
          </span>
        ))}
      </span>
    </Tip>
  );
}
