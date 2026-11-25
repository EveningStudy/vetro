import { Fragment } from "react";
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
  const level = limitLevel(limit);
  if (level === "error") return "text-destructive";
  if (level === "warning") return "text-warning";
  return "text-muted-foreground";
}

export function limitBarTone(limit: RateLimit) {
  const level = limitLevel(limit);
  if (level === "error") return "bg-destructive";
  if (level === "warning") return "bg-warning";
  return "bg-muted-foreground/60";
}

export type UsageTone = "error" | "warning" | null;

export function limitLevel(limit: RateLimit): UsageTone {
  if (limit.usedPercent >= 100) return "error";
  if (limit.usedPercent >= 80) return "warning";
  return null;
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
  const worded = /^(one|two|three|four|five|six|seven|eight|nine|ten|twelve|\d+)[_\s-](hour|day|week)s?(?:[_\s-](.+))?$/.exec(lower);
  if (worded) {
    const numbers: Record<string, number> = { one: 1, two: 2, three: 3, four: 4, five: 5, six: 6, seven: 7, eight: 8, nine: 9, ten: 10, twelve: 12 };
    const count = numbers[worded[1]!] ?? Number(worded[1]);
    const scope = worded[3] ? ` ${worded[3].replace(/_/g, " ")}` : "";
    return `${count}${worded[2]![0]}${scope}`;
  }
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

// Usage chips follow the label / hairline / value rows of T3 Code's ThreadDetailsUsage (MIT).
export const usageChipClass =
  "inline-flex h-6 shrink-0 cursor-default items-center gap-1.5 whitespace-nowrap rounded-lg px-2 text-xs leading-none outline-none transition-colors hover:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring data-popup-open:bg-accent";

export function usageValueClass(tone: UsageTone) {
  return cn(
    "font-medium tabular-nums",
    tone === "error" ? "text-error-foreground" : tone === "warning" ? "text-warning-foreground" : "text-foreground/80",
  );
}

export function usageFillClass(tone: UsageTone) {
  if (tone === "error") return "bg-error";
  if (tone === "warning") return "bg-warning";
  return "bg-foreground/80";
}

export function UsageHairline({ fill, tone, className }: { fill: number; tone: UsageTone; className?: string }) {
  return (
    <span aria-hidden className={cn("relative h-[3px] shrink-0 rounded-full bg-muted-foreground/30 dark:bg-muted-foreground/20", className)}>
      {fill > 0 ? (
        <span className={cn("absolute inset-y-0 left-0 rounded-full", usageFillClass(tone))} style={{ width: `max(${fill}%, 3px)` }} />
      ) : null}
    </span>
  );
}

export function RateLimitBadge({ source }: { source: RateLimitSource }) {
  const t = useT();
  const display = usePrefs((s) => s.usageDisplay);
  const now = useNow(30_000);
  const shown = headline(source.limits);
  const age = snapshotAge(source.updatedAt, now, t);
  const primaryKey = display === "used" ? "usage.used" : "usage.remaining";
  const secondaryKey = display === "used" ? "usage.remaining" : "usage.used";
  const summary = shown.map((limit) => `${shortLimitLabel(limit.label)} ${t(primaryKey, { n: shownPercent(limit, display) })}`).join(", ");
  const details = (
    <div className="flex min-w-0 flex-col gap-1.5 py-0.5">
      <span className="font-medium text-foreground">{t("usage.limits")}</span>
      <div className="grid grid-cols-[auto_2.5rem_auto_auto_auto] items-center gap-x-2.5 gap-y-1 tabular-nums">
        {source.limits.map((limit) => {
          const tone = limitLevel(limit);
          const reset = resetLabel(limit, now, t);
          const other = display === "used" ? remainingPercent(limit) : usedPercent(limit);
          return (
            <Fragment key={limit.label}>
              <span className="max-w-32 truncate text-muted-foreground">{limit.label}</span>
              <UsageHairline fill={shownPercent(limit, display)} tone={tone} className="w-full" />
              <span className={usageValueClass(tone)}>{t(primaryKey, { n: shownPercent(limit, display) })}</span>
              <span className="text-muted-foreground">{t(secondaryKey, { n: other })}</span>
              <span className="text-muted-foreground">{reset ?? ""}</span>
            </Fragment>
          );
        })}
      </div>
      {age || source.provider ? (
        <div className="flex flex-col gap-0.5 text-muted-foreground">
          {age ? <span>{age}</span> : null}
          {source.provider ? <span>{t("usage.routed", { provider: source.provider })}</span> : null}
        </div>
      ) : null}
    </div>
  );
  return (
    <Tip label={details} className="max-w-[min(28rem,calc(100vw-2rem))] px-2.5 py-1.5">
      <span role="img" tabIndex={0} aria-label={`${t("usage.limits")}: ${summary}`} className={cn(usageChipClass, "gap-2.5")}>
        {shown.map((limit) => {
          const tone = limitLevel(limit);
          const percent = shownPercent(limit, display);
          return (
            <span key={limit.label} className="inline-flex items-center gap-1.5">
              <span className="text-muted-foreground">{shortLimitLabel(limit.label)}</span>
              <UsageHairline fill={percent} tone={tone} className="w-5" />
              <span className={usageValueClass(tone)}>{percent}%</span>
            </span>
          );
        })}
      </span>
    </Tip>
  );
}
