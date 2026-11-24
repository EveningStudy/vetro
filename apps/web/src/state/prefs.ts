import { create } from "zustand";

export type ThemePref = "light" | "dark" | "system";
export type LanguagePref = "en" | "zh" | "system";
export type ActivityDays = 7 | 14 | 30 | 0;

/** Everything the UI remembers per device. The core stores it (`prefs.get` / `prefs.set`) so it
 * survives web views without persistent storage; localStorage is only a cache for a fast start. */
export interface PrefsData {
  theme?: ThemePref;
  language?: LanguagePref;
  /** Explicit expand (true) or collapse (false) per project path; missing means the sidebar decides. */
  projectExpanded?: Record<string, boolean>;
  filters?: { agentIds: string[]; sources: string[]; showArchived: boolean; showHidden: boolean; showInternal?: boolean };
  /** Projects active within this many days stay in the main list; 0 keeps all of them. */
  sidebarActivityDays?: ActivityDays;
  hideDisabledAgentHistory?: boolean;
  lastAgent?: string;
  lastProject?: string;
}

const CACHE_KEY = "vetro:prefs";
const query = new URLSearchParams(location.search);

function readCache(): PrefsData {
  try {
    const raw = localStorage.getItem(CACHE_KEY);
    return raw ? (JSON.parse(raw) as PrefsData) : {};
  } catch {
    return {};
  }
}

let data: PrefsData = readCache();
let pushRemote: ((prefs: PrefsData) => void) | null = null;
let pushTimer: ReturnType<typeof setTimeout> | null = null;
const listeners = new Set<(prefs: PrefsData) => void>();

function writeCache() {
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify(data));
  } catch {
    // No persistent storage in this web view; the core keeps the preferences.
  }
}

export function getPrefs(): PrefsData {
  return data;
}

export function savePrefs(patch: Partial<PrefsData>) {
  data = { ...data, ...patch };
  writeCache();
  if (!pushRemote) return;
  if (pushTimer) clearTimeout(pushTimer);
  pushTimer = setTimeout(() => pushRemote?.(data), 300);
}

/** Called once the connection is up. Stored preferences win; if the core has none yet, the local
 * cache is uploaded so nothing the user chose is lost. */
export function connectPrefs(remote: PrefsData | null, push: (prefs: PrefsData) => void) {
  pushRemote = push;
  if (remote && Object.keys(remote).length > 0) {
    data = { ...data, ...remote };
    writeCache();
    for (const listener of listeners) listener(data);
  } else if (Object.keys(data).length > 0) {
    push(data);
  }
  usePrefs.setState(derived(data));
  applyTheme();
}

export function onPrefsLoaded(listener: (prefs: PrefsData) => void) {
  listeners.add(listener);
}

const THEMES = ["light", "dark", "system"] as const;
const LANGUAGES = ["en", "zh", "system"] as const;

function pick<T extends string>(value: string | undefined, allowed: readonly T[], fallback: T, override: string | null): T {
  const chosen = override ?? value;
  return allowed.includes(chosen as T) ? (chosen as T) : fallback;
}

const ACTIVITY_DAYS: readonly ActivityDays[] = [7, 14, 30, 0];

interface UiPrefs {
  theme: ThemePref;
  language: LanguagePref;
  sidebarActivityDays: ActivityDays;
  hideDisabledAgentHistory: boolean;
}

function derived(prefs: PrefsData): UiPrefs {
  return {
    theme: pick(prefs.theme, THEMES, "system", query.get("theme")),
    language: pick(prefs.language, LANGUAGES, "system", query.get("lang")),
    sidebarActivityDays: ACTIVITY_DAYS.includes(prefs.sidebarActivityDays as ActivityDays) ? prefs.sidebarActivityDays! : 14,
    hideDisabledAgentHistory: prefs.hideDisabledAgentHistory ?? true,
  };
}

export const usePrefs = create<UiPrefs>(() => derived(data));

const media = matchMedia("(prefers-color-scheme: dark)");

export function applyTheme() {
  const { theme } = usePrefs.getState();
  const dark = theme === "dark" || (theme === "system" && media.matches);
  const root = document.documentElement;
  if (root.classList.contains("dark") === dark) return;
  root.classList.add("no-transitions");
  root.classList.toggle("dark", dark);
  root.style.backgroundColor = "";
  requestAnimationFrame(() => root.classList.remove("no-transitions"));
}

media.addEventListener("change", applyTheme);

export function setTheme(theme: ThemePref) {
  savePrefs({ theme });
  usePrefs.setState({ theme });
  applyTheme();
}

export function setLanguage(language: LanguagePref) {
  savePrefs({ language });
  usePrefs.setState({ language });
}

export function resolvedLanguage(pref: LanguagePref): "en" | "zh" {
  if (pref !== "system") return pref;
  return navigator.languages.some((lang) => lang.toLowerCase().startsWith("zh")) ? "zh" : "en";
}

export function setSidebarActivityDays(sidebarActivityDays: ActivityDays) {
  savePrefs({ sidebarActivityDays });
  usePrefs.setState({ sidebarActivityDays });
}

export function setHideDisabledAgentHistory(hideDisabledAgentHistory: boolean) {
  savePrefs({ hideDisabledAgentHistory });
  usePrefs.setState({ hideDisabledAgentHistory });
}
