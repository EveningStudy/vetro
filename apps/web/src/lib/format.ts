import type { Translate } from "~/i18n";

export function relativeTime(ts: number | null | undefined, t: Translate, now = Date.now()) {
  if (!ts) return "";
  const seconds = Math.max(0, Math.round((now - ts) / 1000));
  if (seconds < 45) return t("time.now");
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return t("time.minutes", { n: minutes });
  const hours = Math.round(minutes / 60);
  if (hours < 24) return t("time.hours", { n: hours });
  const days = Math.round(hours / 24);
  if (days < 7) return t("time.days", { n: days });
  const weeks = Math.round(days / 7);
  if (days < 30) return t("time.weeks", { n: weeks });
  return new Date(ts).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

export function clockTime(ts: number | null | undefined, withMs = false) {
  if (!ts) return "";
  const date = new Date(ts);
  const base = date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });
  return withMs ? `${base}.${String(date.getMilliseconds()).padStart(3, "0")}` : base;
}

export function formatDuration(ms: number | null | undefined) {
  if (ms == null) return "";
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

export function formatTokens(n: number | null | undefined) {
  if (n == null) return "–";
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0).replace(/\.0$/, "")}k`;
  return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, "")}M`;
}

export function basename(path: string | null | undefined) {
  if (!path) return "";
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;
}

export function tildePath(path: string | null | undefined, home: string | null | undefined) {
  if (!path) return "";
  if (home && path.toLowerCase().startsWith(home.toLowerCase())) return `~${path.slice(home.length)}`;
  return path;
}

export function relativeToCwd(path: string, cwd: string | null | undefined) {
  if (!cwd) return path;
  const normalized = path.replace(/\\/g, "/");
  const root = cwd.replace(/\\/g, "/").replace(/\/$/, "");
  return normalized.toLowerCase().startsWith(`${root.toLowerCase()}/`) ? normalized.slice(root.length + 1) : normalized;
}
