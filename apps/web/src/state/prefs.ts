import { create } from "zustand";
import {
  type CustomTheme,
  type ThemeMode,
  type ThemePaletteTokens,
  DEFAULT_THEME_ID,
  findBuiltInTheme,
  resolveTheme,
  sanitizeCustomTheme,
  themeMode,
  themeTokens,
  writeTokens,
} from "~/styles/themes";

export type ThemePref = "light" | "dark" | "system";
export type LanguagePref = "en" | "zh" | "system";
export type SidebarScope = "all" | "vetro";
export type SidebarOrder = "projects" | "quickChats";
export type FontStyle = "sans" | "serif" | "mono";
export type FontScale = "compact" | "default" | "large";
export type ChatWidth = "standard" | "wide" | "full";
export type BypassMemoryScope = "project" | "global";

export interface AgentOptionMemory {
  options?: Record<string, string>;
  byModel?: Record<string, Record<string, string>>;
  danger?: Record<string, string>;
  dangerByProject?: Record<string, Record<string, string>>;
}

/** Everything the UI remembers per device. The core stores it (`prefs.get` / `prefs.set`) so it
 * survives web views without persistent storage; localStorage is only a cache for a fast start. */
export interface PrefsData {
  theme?: ThemePref;
  language?: LanguagePref;
  /** Explicit expand (true) or collapse (false) per project path; missing means the sidebar decides. */
  projectExpanded?: Record<string, boolean>;
  filters?: { agentIds: string[]; sources: string[]; showArchived: boolean; showHidden: boolean; showInternal?: boolean };
  /** Projects active within this many days stay in the main list; 0 keeps all of them. */
  sidebarActivityDays?: number;
  sidebarCollapsed?: boolean;
  /** `vetro` limits the sidebar to threads started in Vetro; search still covers everything. */
  sidebarScope?: SidebarScope;
  /** Which of Projects and Quick chats comes first in the sidebar. */
  sidebarOrder?: SidebarOrder;
  /** Explicit open (true) or closed (false) state of sidebar groups (older, missing, automated runs). */
  sidebarGroupsOpen?: Record<string, boolean>;
  drawerWidth?: number;
  hideDisabledAgentHistory?: boolean;
  lastAgent?: string;
  lastProject?: string;
  themeId?: string;
  customThemes?: CustomTheme[];
  fontStyle?: FontStyle;
  fontScale?: FontScale;
  chatWidth?: ChatWidth;
  paperGrain?: boolean;
  /** Opaque background of the active palette; the desktop shell paints its window with it. */
  themeBg?: string;
  /** Where dangerous/bypass permission modes are remembered: per project (default) or globally per agent. */
  bypassMemoryScope?: BypassMemoryScope;
  /** Remembered non-default options per agent id. */
  agentOptions?: Record<string, AgentOptionMemory>;
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
const FONT_STYLES = ["sans", "serif", "mono"] as const;
const FONT_SCALES = ["compact", "default", "large"] as const;
const CHAT_WIDTHS = ["standard", "wide", "full"] as const;

function pick<T extends string>(value: string | undefined, allowed: readonly T[], fallback: T, override: string | null): T {
  const chosen = override ?? value;
  return allowed.includes(chosen as T) ? (chosen as T) : fallback;
}

export const DRAWER_MIN = 340;
export const DRAWER_MAX = 960;

export function clampDrawerWidth(width: number) {
  return Math.round(Math.min(DRAWER_MAX, Math.max(DRAWER_MIN, width)));
}

function activityDays(value: unknown) {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? Math.round(value) : 14;
}

function customThemes(value: unknown): CustomTheme[] {
  if (!Array.isArray(value)) return [];
  return value.map(sanitizeCustomTheme).filter((theme): theme is CustomTheme => theme !== null);
}

interface UiPrefs {
  theme: ThemePref;
  language: LanguagePref;
  sidebarActivityDays: number;
  hideDisabledAgentHistory: boolean;
  sidebarCollapsed: boolean;
  sidebarScope: SidebarScope;
  sidebarOrder: SidebarOrder;
  sidebarGroupsOpen: Record<string, boolean>;
  drawerWidth: number;
  themeId: string;
  customThemes: CustomTheme[];
  fontStyle: FontStyle;
  fontScale: FontScale;
  chatWidth: ChatWidth;
  paperGrain: boolean;
  bypassMemoryScope: BypassMemoryScope;
  systemDark: boolean;
}

const media = matchMedia("(prefers-color-scheme: dark)");

/** `?palette=<theme id>` previews a theme for one session (screenshots); other values are ignored. */
function paletteOverride(prefs: PrefsData): string | null {
  const id = query.get("palette");
  if (!id) return null;
  return findBuiltInTheme(id) || prefs.customThemes?.some((theme) => theme?.id === id) ? id : null;
}

function derived(prefs: PrefsData): UiPrefs {
  return {
    theme: pick(prefs.theme, THEMES, "system", query.get("theme")),
    language: pick(prefs.language, LANGUAGES, "system", query.get("lang")),
    sidebarActivityDays: activityDays(prefs.sidebarActivityDays),
    hideDisabledAgentHistory: prefs.hideDisabledAgentHistory ?? true,
    sidebarCollapsed: prefs.sidebarCollapsed ?? false,
    sidebarScope: prefs.sidebarScope === "vetro" ? "vetro" : "all",
    sidebarOrder: prefs.sidebarOrder === "quickChats" ? "quickChats" : "projects",
    sidebarGroupsOpen: prefs.sidebarGroupsOpen && typeof prefs.sidebarGroupsOpen === "object" ? prefs.sidebarGroupsOpen : {},
    drawerWidth: clampDrawerWidth(typeof prefs.drawerWidth === "number" ? prefs.drawerWidth : 480),
    themeId: paletteOverride(prefs) ?? (typeof prefs.themeId === "string" && prefs.themeId ? prefs.themeId : DEFAULT_THEME_ID),
    customThemes: customThemes(prefs.customThemes),
    fontStyle: pick(prefs.fontStyle, FONT_STYLES, "sans", null),
    fontScale: pick(prefs.fontScale, FONT_SCALES, "default", null),
    chatWidth: pick(prefs.chatWidth, CHAT_WIDTHS, "standard", null),
    paperGrain: prefs.paperGrain === true,
    bypassMemoryScope: prefs.bypassMemoryScope === "global" ? "global" : "project",
    systemDark: media.matches,
  };
}

export const usePrefs = create<UiPrefs>(() => derived(data));

const FONT_FAMILIES: Record<FontStyle, string> = { sans: "var(--font-sans)", serif: "var(--font-serif)", mono: "var(--font-mono)" };
const FONT_SIZES: Record<FontScale, string> = { compact: "0.8125rem", default: "0.875rem", large: "0.96875rem" };
const CHAT_MAX_WIDTHS: Record<ChatWidth, string> = { standard: "46rem", wide: "60rem", full: "100%" };
const BOOT_BG_KEY = "vetro:boot-bg";
const BOOT_MODE_KEY = "vetro:boot-mode";

let preview: { mode: ThemeMode; tokens: ThemePaletteTokens } | null = null;

export function preferredMode(theme: ThemePref, systemDark: boolean): ThemeMode {
  return theme === "dark" || (theme === "system" && systemDark) ? "dark" : "light";
}

/** The mode and palette the saved preferences select (ignoring any editor preview). */
function savedPalette() {
  const state = usePrefs.getState();
  const resolved = resolveTheme(state.themeId, state.customThemes);
  const preferred = preferredMode(state.theme, state.systemDark);
  const mode = themeMode(resolved, preferred);
  return {
    mode,
    tokens: themeTokens(resolved, mode),
    forced: resolved.kind === "custom" && resolved.theme.appearance !== "both",
    isDefault: resolved.kind === "builtin" && resolved.theme.id === DEFAULT_THEME_ID,
  };
}

export function applyTheme() {
  const state = usePrefs.getState();
  const saved = savedPalette();
  const { mode, tokens } = preview ?? saved;
  const root = document.documentElement;
  const dark = mode === "dark";
  const switching = root.classList.contains("dark") !== dark;
  if (switching) {
    root.classList.add("no-transitions");
    root.classList.toggle("dark", dark);
  }
  writeTokens(root, !preview && saved.isDefault ? null : tokens);
  root.style.backgroundColor = "";
  root.dataset.fontStyle = state.fontStyle;
  root.dataset.fontScale = state.fontScale;
  root.style.setProperty("--chat-font-family", FONT_FAMILIES[state.fontStyle]);
  root.style.setProperty("--chat-font-size", FONT_SIZES[state.fontScale]);
  root.style.setProperty("--chat-content-max-width", CHAT_MAX_WIDTHS[state.chatWidth]);
  if (state.paperGrain) root.dataset.paperGrain = "true";
  else delete root.dataset.paperGrain;
  if (switching) requestAnimationFrame(() => root.classList.remove("no-transitions"));
  if (paletteOverride(data) !== null) return;
  const bgHex = saved.tokens.bgHex;
  try {
    localStorage.setItem(BOOT_BG_KEY, bgHex);
    localStorage.setItem(BOOT_MODE_KEY, saved.forced ? `forced-${saved.mode}` : saved.mode);
  } catch {
    // No persistent storage; the boot script falls back to the default background.
  }
  if (data.themeBg !== bgHex) savePrefs({ themeBg: bgHex });
}

/** Shows a palette on the live app without saving it, until `clearThemePreview`. */
export function previewThemeTokens(mode: ThemeMode, tokens: ThemePaletteTokens) {
  preview = { mode, tokens };
  applyTheme();
}

export function clearThemePreview() {
  if (!preview) return;
  preview = null;
  applyTheme();
}

media.addEventListener("change", () => {
  usePrefs.setState({ systemDark: media.matches });
  applyTheme();
});

export function setTheme(theme: ThemePref) {
  savePrefs({ theme });
  usePrefs.setState({ theme });
  applyTheme();
}

export function setThemeId(themeId: string) {
  savePrefs({ themeId });
  usePrefs.setState({ themeId });
  applyTheme();
}

export function saveCustomTheme(theme: CustomTheme) {
  const current = usePrefs.getState().customThemes;
  const customThemes = current.some((entry) => entry.id === theme.id) ? current.map((entry) => (entry.id === theme.id ? theme : entry)) : [...current, theme];
  savePrefs({ customThemes });
  usePrefs.setState({ customThemes });
  applyTheme();
}

export function deleteCustomTheme(id: string) {
  const state = usePrefs.getState();
  const customThemes = state.customThemes.filter((entry) => entry.id !== id);
  const themeId = state.themeId === id ? DEFAULT_THEME_ID : state.themeId;
  savePrefs(themeId === state.themeId ? { customThemes } : { customThemes, themeId });
  usePrefs.setState({ customThemes, themeId });
  applyTheme();
}

export function setFontStyle(fontStyle: FontStyle) {
  savePrefs({ fontStyle });
  usePrefs.setState({ fontStyle });
  applyTheme();
}

export function setFontScale(fontScale: FontScale) {
  savePrefs({ fontScale });
  usePrefs.setState({ fontScale });
  applyTheme();
}

export function setChatWidth(chatWidth: ChatWidth) {
  savePrefs({ chatWidth });
  usePrefs.setState({ chatWidth });
  applyTheme();
}

export function setPaperGrain(paperGrain: boolean) {
  savePrefs({ paperGrain });
  usePrefs.setState({ paperGrain });
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

export function setSidebarActivityDays(days: number) {
  const sidebarActivityDays = activityDays(days);
  savePrefs({ sidebarActivityDays });
  usePrefs.setState({ sidebarActivityDays });
}

export function setHideDisabledAgentHistory(hideDisabledAgentHistory: boolean) {
  savePrefs({ hideDisabledAgentHistory });
  usePrefs.setState({ hideDisabledAgentHistory });
}

export function setSidebarCollapsed(sidebarCollapsed: boolean) {
  savePrefs({ sidebarCollapsed });
  usePrefs.setState({ sidebarCollapsed });
}

export function setSidebarScope(sidebarScope: SidebarScope) {
  savePrefs({ sidebarScope });
  usePrefs.setState({ sidebarScope });
}

export function setSidebarOrder(sidebarOrder: SidebarOrder) {
  savePrefs({ sidebarOrder });
  usePrefs.setState({ sidebarOrder });
}

export function setSidebarGroupOpen(key: string, open: boolean) {
  const sidebarGroupsOpen = { ...usePrefs.getState().sidebarGroupsOpen, [key]: open };
  savePrefs({ sidebarGroupsOpen });
  usePrefs.setState({ sidebarGroupsOpen });
}

export function setDrawerWidth(width: number) {
  const drawerWidth = clampDrawerWidth(width);
  savePrefs({ drawerWidth });
  usePrefs.setState({ drawerWidth });
}

export function setBypassMemoryScope(bypassMemoryScope: BypassMemoryScope) {
  savePrefs({ bypassMemoryScope });
  usePrefs.setState({ bypassMemoryScope });
}

function normCwd(cwd: string | null | undefined) {
  return cwd ? cwd.replace(/^\\\\\?\\/, "").replace(/[\\/]+$/, "").toLowerCase() : null;
}

/** Resolves remembered overrides for an agent against its currently reported options. */
export function resolveSavedAgentOptions(
  agentId: string,
  cwd: string | null,
  options: { id: string; category: string; currentValue: string; choices: { value: string; danger: boolean }[] }[],
  scope: BypassMemoryScope,
): Record<string, string> {
  const memory = data.agentOptions?.[agentId];
  if (!memory) return {};
  const out: Record<string, string> = {};
  const valid = (option: (typeof options)[number], value: string | undefined) =>
    !!value && option.choices.some((choice) => choice.value === value);

  const modelOption = options.find((option) => option.category === "model");
  let activeModel = modelOption?.currentValue ?? "";
  if (modelOption) {
    const savedModel = memory.options?.[modelOption.id];
    if (valid(modelOption, savedModel) && savedModel !== modelOption.currentValue) {
      out[modelOption.id] = savedModel!;
      activeModel = savedModel!;
    }
  }

  const projectKey = normCwd(cwd);
  for (const option of options) {
    if (option.category === "model") continue;
    let candidate: string | undefined;
    if (option.category === "thought_level" && activeModel) {
      candidate = memory.byModel?.[activeModel]?.[option.id];
    }
    if (!candidate) {
      const dangerSource = scope === "global" ? memory.danger : projectKey ? memory.dangerByProject?.[projectKey] : undefined;
      candidate = dangerSource?.[option.id] ?? memory.options?.[option.id];
    }
    if (!valid(option, candidate) || candidate === option.currentValue) continue;
    const choice = option.choices.find((entry) => entry.value === candidate);
    if (choice?.danger && scope === "project" && !projectKey) continue;
    out[option.id] = candidate!;
  }
  return out;
}

/** Saves an explicit user option change (or removes it when restored to the agent's default). */
export function rememberAgentOption(
  agentId: string,
  cwd: string | null,
  option: { id: string; category: string; choices: { value: string; danger: boolean }[] },
  value: string,
  defaultValue: string | undefined,
  activeModel: string | undefined,
  scope: BypassMemoryScope,
) {
  const all = { ...(data.agentOptions ?? {}) };
  const prev = all[agentId] ?? {};
  const next: AgentOptionMemory = {
    options: { ...(prev.options ?? {}) },
    byModel: { ...(prev.byModel ?? {}) },
    danger: { ...(prev.danger ?? {}) },
    dangerByProject: { ...(prev.dangerByProject ?? {}) },
  };
  const isDefault = value === defaultValue;
  const choice = option.choices.find((entry) => entry.value === value);
  const projectKey = normCwd(cwd);

  if (option.category === "thought_level" && activeModel) {
    const map = { ...(next.byModel![activeModel] ?? {}) };
    if (isDefault) delete map[option.id];
    else map[option.id] = value;
    if (Object.keys(map).length === 0) delete next.byModel![activeModel];
    else next.byModel![activeModel] = map;
  } else if (choice?.danger) {
    delete next.options![option.id];
    if (scope === "global") {
      if (isDefault) delete next.danger![option.id];
      else next.danger![option.id] = value;
    } else if (projectKey) {
      const map = { ...(next.dangerByProject![projectKey] ?? {}) };
      if (isDefault) delete map[option.id];
      else map[option.id] = value;
      if (Object.keys(map).length === 0) delete next.dangerByProject![projectKey];
      else next.dangerByProject![projectKey] = map;
    }
  } else {
    // Switching away from a danger mode clears any stored danger override for this option.
    delete next.danger![option.id];
    if (projectKey && next.dangerByProject![projectKey]) {
      const map = { ...next.dangerByProject![projectKey] };
      delete map[option.id];
      if (Object.keys(map).length === 0) delete next.dangerByProject![projectKey];
      else next.dangerByProject![projectKey] = map;
    }
    if (isDefault) delete next.options![option.id];
    else next.options![option.id] = value;
  }

  all[agentId] = next;
  savePrefs({ agentOptions: all });
}
