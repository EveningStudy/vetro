// The T3 Chat, Grove, Ocean, Ember and Iris palettes and the ThemeFile v1 import format are adapted
// from T3 Code (MIT, https://github.com/pingdotgg/t3code); see THIRD_PARTY_NOTICES.md.

export type ThemeMode = "light" | "dark";
export type ThemeAppearance = ThemeMode | "both";
export type ThemeCategory = "paper" | "t3";

export interface ThemePaletteTokens {
  bgHex: string;
  background: string;
  sidebar: string;
  card: string;
  surfaceRaised: string;
  popover: string;
  foreground: string;
  mutedForeground: string;
  primary: string;
  primaryForeground: string;
  secondary: string;
  secondaryForeground: string;
  muted: string;
  accent: string;
  accentForeground: string;
  border: string;
  input: string;
  ring: string;
  messageSurface: string;
  messageForeground: string;
  messageAction: string;
  messageActionForeground: string;
  messageActionHover: string;
  codeBackground: string;
  sidebarForeground: string;
  sidebarMutedForeground: string;
  sidebarRowHover: string;
  sidebarRowActive: string;
  sidebarBorder: string;
  scrollbarThumb: string;
  scrollbarThumbHover: string;
  error?: string;
  errorForeground?: string;
  errorSurface?: string;
  warning?: string;
  warningForeground?: string;
  warningSurface?: string;
  info?: string;
  infoForeground?: string;
  success?: string;
  successForeground?: string;
  diffAddition?: string;
  diffDeletion?: string;
  diffAdditionForeground?: string;
  diffDeletionForeground?: string;
}

export type ThemeColorRole = Exclude<keyof ThemePaletteTokens, "bgHex">;
export type ThemeSwatch = [background: string, sidebar: string, accent: string, text: string];

export interface BuiltInTheme {
  id: string;
  nameKey: BuiltInThemeNameKey;
  category: ThemeCategory;
  swatch: Record<ThemeMode, ThemeSwatch>;
  light: ThemePaletteTokens;
  dark: ThemePaletteTokens;
}

export interface CustomThemeSeed {
  background: string;
  sidebar: string;
  primary: string;
  foreground: string;
  card: string;
  messageSurface: string;
}

export interface CustomTheme {
  id: string;
  name: string;
  appearance: ThemeAppearance;
  light?: CustomThemeSeed;
  dark?: CustomThemeSeed;
  overrides?: Partial<Record<ThemeMode, Partial<ThemePaletteTokens>>>;
}

export type BuiltInThemeNameKey =
  | "theme.name.paper"
  | "theme.name.parchment"
  | "theme.name.solarized"
  | "theme.name.rosePine"
  | "theme.name.default"
  | "theme.name.t3Chat"
  | "theme.name.grove"
  | "theme.name.ocean"
  | "theme.name.ember"
  | "theme.name.iris";

export const SEED_ROLES = ["background", "sidebar", "primary", "foreground", "card", "messageSurface"] as const satisfies readonly (keyof CustomThemeSeed)[];
export type SeedRole = (typeof SEED_ROLES)[number];

/** Roles derived from each seed colour. Editing a seed drops imported overrides for these roles,
 * so the change shows instead of being masked by colours tuned for the old seed. */
export const SEED_DEPENDENTS: Record<SeedRole, ThemeColorRole[]> = {
  background: ["secondary", "muted", "accent", "border", "input", "codeBackground", "scrollbarThumb", "scrollbarThumbHover", "messageActionHover"],
  sidebar: ["sidebarRowHover", "sidebarRowActive", "sidebarBorder"],
  primary: ["primaryForeground", "ring", "messageAction", "messageActionForeground", "messageActionHover"],
  foreground: [
    "mutedForeground",
    "secondaryForeground",
    "accentForeground",
    "messageForeground",
    "sidebarForeground",
    "sidebarMutedForeground",
    "border",
    "input",
    "secondary",
    "muted",
    "accent",
    "scrollbarThumb",
    "scrollbarThumbHover",
  ],
  card: ["surfaceRaised", "popover", "codeBackground"],
  messageSurface: [],
};

/** Every palette role and the custom property it drives. */
export const TOKEN_VARS: Record<ThemeColorRole, string[]> = {
  background: ["--background"],
  sidebar: ["--sidebar"],
  card: ["--card"],
  surfaceRaised: ["--surface-raised"],
  popover: ["--popover"],
  foreground: ["--foreground", "--card-foreground", "--popover-foreground"],
  mutedForeground: ["--muted-foreground", "--placeholder", "--secondary-label", "--icon-muted"],
  primary: ["--primary"],
  primaryForeground: ["--primary-foreground"],
  secondary: ["--secondary"],
  secondaryForeground: ["--secondary-foreground"],
  muted: ["--muted"],
  accent: ["--accent"],
  accentForeground: ["--accent-foreground"],
  border: ["--border"],
  input: ["--input"],
  ring: ["--ring"],
  messageSurface: ["--message-surface"],
  messageForeground: ["--message-foreground"],
  messageAction: ["--message-action"],
  messageActionForeground: ["--message-action-foreground"],
  messageActionHover: ["--message-action-hover"],
  codeBackground: ["--code-background"],
  sidebarForeground: ["--sidebar-foreground"],
  sidebarMutedForeground: ["--sidebar-muted-foreground"],
  sidebarRowHover: ["--sidebar-row-hover"],
  sidebarRowActive: ["--sidebar-row-active"],
  sidebarBorder: ["--sidebar-border"],
  scrollbarThumb: ["--app-scrollbar-thumb"],
  scrollbarThumbHover: ["--app-scrollbar-thumb-hover"],
  error: ["--error", "--destructive"],
  errorForeground: ["--error-foreground", "--destructive-foreground"],
  errorSurface: ["--error-surface"],
  warning: ["--warning"],
  warningForeground: ["--warning-foreground"],
  warningSurface: ["--warning-surface"],
  info: ["--info"],
  infoForeground: ["--info-foreground"],
  success: ["--success"],
  successForeground: ["--success-foreground"],
  diffAddition: ["--diff-addition"],
  diffDeletion: ["--diff-deletion"],
  diffAdditionForeground: ["--diff-addition-foreground"],
  diffDeletionForeground: ["--diff-deletion-foreground"],
};

export const COLOR_ROLES = Object.keys(TOKEN_VARS) as ThemeColorRole[];
export const MANAGED_VARS = [...new Set(Object.values(TOKEN_VARS).flat())];

const mix = (a: string, share: number, b: string) => `color-mix(in srgb, ${a} ${share}%, ${b})`;

function hexChannels(color: string): [number, number, number] | null {
  const match = /^#([0-9a-f]{6})$/i.exec(color.trim());
  if (!match) return null;
  const value = Number.parseInt(match[1]!, 16);
  return [(value >> 16) & 255, (value >> 8) & 255, value & 255];
}

function luminance(color: string): number | null {
  const channels = hexChannels(color);
  if (!channels) return null;
  const [r, g, b] = channels.map((channel) => {
    const c = channel / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  }) as [number, number, number];
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

export function contrastRatio(a: string, b: string): number | null {
  const la = luminance(a);
  const lb = luminance(b);
  if (la === null || lb === null) return null;
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

/** Picks the readable text colour for a filled accent. Only hex accents can be measured; other
 * colour syntaxes fall back to the usual choice for the mode. */
function onColor(fill: string, mode: ThemeMode, light: string, dark: string) {
  const l = luminance(fill);
  if (l === null) return mode === "dark" ? dark : light;
  return l > 0.36 ? dark : light;
}

type BaseTokens = Pick<ThemePaletteTokens, "background" | "foreground" | "primary"> & Partial<ThemePaletteTokens>;

/** Fills every role a palette leaves out with colours mixed from the ones it has. */
export function completeTokens(base: BaseTokens, mode: ThemeMode): ThemePaletteTokens {
  const dark = mode === "dark";
  const { background: bg, foreground: fg, primary } = base;
  const sidebar = base.sidebar ?? (dark ? mix(fg, 3, bg) : mix(fg, 2.5, bg));
  const card = base.card ?? (dark ? mix(fg, 4, bg) : mix("#ffffff", 60, bg));
  const surfaceRaised = base.surfaceRaised ?? card;
  const primaryForeground = base.primaryForeground ?? onColor(primary, mode, "#ffffff", hexChannels(bg) ? bg : "#111111");
  const mutedForeground = base.mutedForeground ?? mix(fg, dark ? 64 : 62, bg);
  return {
    ...base,
    bgHex: base.bgHex ?? (hexChannels(bg) ? bg.toLowerCase() : dark ? "#111111" : "#fafafa"),
    background: bg,
    sidebar,
    card,
    surfaceRaised,
    popover: base.popover ?? surfaceRaised,
    foreground: fg,
    mutedForeground,
    primary,
    primaryForeground,
    secondary: base.secondary ?? mix(fg, dark ? 6 : 5, bg),
    secondaryForeground: base.secondaryForeground ?? fg,
    muted: base.muted ?? mix(fg, dark ? 5 : 4, bg),
    accent: base.accent ?? mix(fg, dark ? 9 : 7, bg),
    accentForeground: base.accentForeground ?? fg,
    border: base.border ?? mix(fg, dark ? 13 : 12, bg),
    input: base.input ?? mix(fg, dark ? 19 : 18, bg),
    ring: base.ring ?? primary,
    messageSurface: base.messageSurface ?? mix(fg, dark ? 10 : 8, bg),
    messageForeground: base.messageForeground ?? fg,
    messageAction: base.messageAction ?? primary,
    messageActionForeground: base.messageActionForeground ?? primaryForeground,
    messageActionHover: base.messageActionHover ?? mix(primary, 90, bg),
    codeBackground: base.codeBackground ?? mix(card, 70, bg),
    sidebarForeground: base.sidebarForeground ?? fg,
    sidebarMutedForeground: base.sidebarMutedForeground ?? mutedForeground,
    sidebarRowHover: base.sidebarRowHover ?? mix(fg, dark ? 7 : 6, sidebar),
    sidebarRowActive: base.sidebarRowActive ?? (dark ? mix(fg, 11, sidebar) : mix("#ffffff", 70, sidebar)),
    sidebarBorder: base.sidebarBorder ?? base.border ?? mix(fg, dark ? 11 : 10, sidebar),
    scrollbarThumb: base.scrollbarThumb ?? mix(fg, dark ? 26 : 18, "transparent"),
    scrollbarThumbHover: base.scrollbarThumbHover ?? mix(fg, dark ? 38 : 30, "transparent"),
  };
}

export function deriveTokensFromSeed(seed: CustomThemeSeed, mode: ThemeMode, overrides?: Partial<ThemePaletteTokens>): ThemePaletteTokens {
  const derived = completeTokens(
    {
      background: seed.background,
      sidebar: seed.sidebar,
      primary: seed.primary,
      foreground: seed.foreground,
      card: seed.card,
      messageSurface: seed.messageSurface,
    },
    mode,
  );
  const merged: ThemePaletteTokens = { ...derived, ...overrides };
  merged.bgHex = resolveColorHex(merged.background) ?? derived.bgHex;
  return merged;
}

function builtIn(id: string, nameKey: BuiltInThemeNameKey, category: ThemeCategory, light: BaseTokens & { bgHex: string }, dark: BaseTokens & { bgHex: string }): BuiltInTheme {
  const lightTokens = completeTokens(light, "light");
  const darkTokens = completeTokens(dark, "dark");
  const swatch = (tokens: ThemePaletteTokens): ThemeSwatch => [tokens.background, tokens.sidebar, tokens.primary, tokens.foreground];
  return { id, nameKey, category, swatch: { light: swatch(lightTokens), dark: swatch(darkTokens) }, light: lightTokens, dark: darkTokens };
}

const T3_LIGHT_TEXT = "oklch(0.222003 0.03479 328.979)";
const T3_DARK_TEXT = "oklch(0.990339 0.008411 325.64)";

export const BUILT_IN_THEMES: BuiltInTheme[] = [
  builtIn(
    "paper",
    "theme.name.paper",
    "paper",
    {
      bgHex: "#faf7f0",
      background: "#faf7f0",
      sidebar: "#f2ede2",
      card: "#fffdf8",
      surfaceRaised: "#fffdf8",
      popover: "#fffdf8",
      foreground: "#2b2723",
      mutedForeground: "#736b61",
      primary: "#c96442",
      primaryForeground: "#fffdf8",
      secondary: "#efe9dc",
      secondaryForeground: "#2b2723",
      muted: "#f1ebde",
      accent: "#e9e2d3",
      accentForeground: "#2b2723",
      border: "#e3dbcb",
      input: "#d6ccb8",
      ring: "#c96442",
      messageSurface: "#ede6d8",
      messageForeground: "#2b2723",
      codeBackground: "#f5f0e5",
      sidebarForeground: "#2b2723",
      sidebarMutedForeground: "#736b61",
      sidebarRowHover: "#e9e2d3",
      sidebarRowActive: "#fbf8f2",
      sidebarBorder: "#e3dbcb",
      scrollbarThumb: "#d9d0bf",
      scrollbarThumbHover: "#c4b9a5",
      infoForeground: "#2f6690",
    },
    {
      bgHex: "#1c1917",
      background: "#1c1917",
      sidebar: "#151311",
      card: "#24201d",
      surfaceRaised: "#24201d",
      popover: "#26221f",
      foreground: "#ede6da",
      mutedForeground: "#a39889",
      primary: "#dc7b58",
      primaryForeground: "#1c1917",
      secondary: "#2b2622",
      secondaryForeground: "#ede6da",
      muted: "#27231f",
      accent: "#322c27",
      accentForeground: "#ede6da",
      border: "#342e29",
      input: "#423a33",
      ring: "#dc7b58",
      messageSurface: "#2c2723",
      messageForeground: "#ede6da",
      codeBackground: "#221e1b",
      sidebarForeground: "#ede6da",
      sidebarMutedForeground: "#9e9284",
      sidebarRowHover: "#24201d",
      sidebarRowActive: "#2e2924",
      sidebarBorder: "#2d2722",
      scrollbarThumb: "#4a423b",
      scrollbarThumbHover: "#5e544b",
    },
  ),
  builtIn(
    "parchment",
    "theme.name.parchment",
    "paper",
    {
      bgHex: "#f5edd8",
      background: "#f5edd8",
      sidebar: "#ece1c6",
      card: "#faf4e4",
      surfaceRaised: "#faf4e4",
      popover: "#fbf6e8",
      foreground: "#332a1e",
      mutedForeground: "#766750",
      primary: "#a86220",
      primaryForeground: "#fdf9ef",
      secondary: "#e9ddb9",
      secondaryForeground: "#332a1e",
      muted: "#ece2c7",
      accent: "#e2d4b0",
      accentForeground: "#332a1e",
      border: "#d9cba7",
      input: "#cab98f",
      ring: "#a86220",
      messageSurface: "#e8dcbe",
      messageForeground: "#332a1e",
      codeBackground: "#efe5cb",
      sidebarForeground: "#332a1e",
      sidebarMutedForeground: "#766750",
      sidebarRowHover: "#e2d5b3",
      sidebarRowActive: "#f7f0dd",
      sidebarBorder: "#d9cba7",
      scrollbarThumb: "#cfbf98",
      scrollbarThumbHover: "#b9a77d",
      infoForeground: "#335f86",
    },
    {
      bgHex: "#1f1b14",
      background: "#1f1b14",
      sidebar: "#18150f",
      card: "#282219",
      surfaceRaised: "#282219",
      popover: "#2b251b",
      foreground: "#eadfc9",
      mutedForeground: "#a8987e",
      primary: "#dfa04b",
      primaryForeground: "#1f1b14",
      secondary: "#30291e",
      secondaryForeground: "#eadfc9",
      muted: "#2b241a",
      accent: "#382f22",
      accentForeground: "#eadfc9",
      border: "#3b3224",
      input: "#4a3e2c",
      ring: "#dfa04b",
      messageSurface: "#31291d",
      messageForeground: "#eadfc9",
      codeBackground: "#252017",
      sidebarForeground: "#eadfc9",
      sidebarMutedForeground: "#a39379",
      sidebarRowHover: "#282219",
      sidebarRowActive: "#342c20",
      sidebarBorder: "#322a1e",
      scrollbarThumb: "#4d4231",
      scrollbarThumbHover: "#62553f",
    },
  ),
  builtIn(
    "solarized",
    "theme.name.solarized",
    "paper",
    {
      bgHex: "#fdf6e3",
      background: "#fdf6e3",
      sidebar: "#eee8d5",
      card: "#fffbf0",
      surfaceRaised: "#fffbf0",
      popover: "#fffcf2",
      foreground: "#3b4c52",
      mutedForeground: "#5c7179",
      primary: "#268bd2",
      primaryForeground: "#fdf6e3",
      secondary: "#eee8d5",
      secondaryForeground: "#3b4c52",
      muted: "#f2ecda",
      accent: "#e6dfc9",
      accentForeground: "#073642",
      border: "#dfd7c0",
      input: "#cfc6ad",
      ring: "#268bd2",
      messageSurface: "#eee8d5",
      messageForeground: "#3b4c52",
      codeBackground: "#f5eedb",
      sidebarForeground: "#3b4c52",
      sidebarMutedForeground: "#5c7179",
      sidebarRowHover: "#e5dec8",
      sidebarRowActive: "#fdf6e3",
      sidebarBorder: "#dfd7c0",
      scrollbarThumb: "#d6cdb3",
      scrollbarThumbHover: "#bfb597",
      infoForeground: "#1f6fa8",
    },
    {
      bgHex: "#002b36",
      background: "#002b36",
      sidebar: "#073642",
      card: "#06333f",
      surfaceRaised: "#073642",
      popover: "#093c49",
      foreground: "#eee8d5",
      mutedForeground: "#93a1a1",
      primary: "#2aa198",
      primaryForeground: "#002b36",
      secondary: "#0a3f4d",
      secondaryForeground: "#eee8d5",
      muted: "#073642",
      accent: "#0d4857",
      accentForeground: "#fdf6e3",
      border: "#114957",
      input: "#195a6b",
      ring: "#2aa198",
      messageSurface: "#0a3e4b",
      messageForeground: "#eee8d5",
      codeBackground: "#04303c",
      sidebarForeground: "#eee8d5",
      sidebarMutedForeground: "#93a1a1",
      sidebarRowHover: "#0c4250",
      sidebarRowActive: "#114e5e",
      sidebarBorder: "#104553",
      scrollbarThumb: "#1d5868",
      scrollbarThumbHover: "#2a6b7c",
      infoForeground: "#6cb6eb",
    },
  ),
  builtIn(
    "rose-pine",
    "theme.name.rosePine",
    "paper",
    {
      bgHex: "#faf4ed",
      background: "#faf4ed",
      sidebar: "#f2e9e1",
      card: "#fffaf3",
      surfaceRaised: "#fffaf3",
      popover: "#fffaf3",
      foreground: "#575279",
      mutedForeground: "#6e6a88",
      primary: "#b4637a",
      primaryForeground: "#fffaf3",
      secondary: "#f2e9e1",
      secondaryForeground: "#575279",
      muted: "#f4ede8",
      accent: "#dfdad9",
      accentForeground: "#575279",
      border: "#e4dcd5",
      input: "#d4cbc4",
      ring: "#b4637a",
      messageSurface: "#efe6de",
      messageForeground: "#575279",
      codeBackground: "#f5eee7",
      sidebarForeground: "#575279",
      sidebarMutedForeground: "#6e6a88",
      sidebarRowHover: "#e9dfd7",
      sidebarRowActive: "#fffaf3",
      sidebarBorder: "#e4dcd5",
      scrollbarThumb: "#dcd2ca",
      scrollbarThumbHover: "#c7bcb4",
      infoForeground: "#286983",
    },
    {
      bgHex: "#232136",
      background: "#232136",
      sidebar: "#1f1d2e",
      card: "#2a273f",
      surfaceRaised: "#2a273f",
      popover: "#2d2a45",
      foreground: "#e0def4",
      mutedForeground: "#908caa",
      primary: "#ea9a97",
      primaryForeground: "#232136",
      secondary: "#302d47",
      secondaryForeground: "#e0def4",
      muted: "#2a273f",
      accent: "#393552",
      accentForeground: "#e0def4",
      border: "#3d3958",
      input: "#4d486e",
      ring: "#ea9a97",
      messageSurface: "#312d4a",
      messageForeground: "#e0def4",
      codeBackground: "#27243b",
      sidebarForeground: "#e0def4",
      sidebarMutedForeground: "#908caa",
      sidebarRowHover: "#2a273f",
      sidebarRowActive: "#34304c",
      sidebarBorder: "#34304b",
      scrollbarThumb: "#47426a",
      scrollbarThumbHover: "#59537f",
      infoForeground: "#9ccfd8",
    },
  ),
  builtIn(
    "default",
    "theme.name.default",
    "t3",
    {
      bgHex: "#fcfcfc",
      background: "#fcfcfc",
      sidebar: "#fafafa",
      card: "#ffffff",
      surfaceRaised: "#ffffff",
      popover: "#ffffff",
      foreground: "#18181b",
      mutedForeground: "#71717a",
      primary: "oklch(0.488 0.217 264)",
      primaryForeground: "#ffffff",
      secondary: "#fafafa",
      secondaryForeground: "#27272a",
      muted: "#fafafa",
      accent: "#f4f4f5",
      accentForeground: "#18181b",
      border: "#e4e4e7",
      input: "#d4d4d8",
      messageSurface: "#ececee",
      codeBackground: "#fefefe",
      sidebarRowHover: "#f0f0f1",
      sidebarRowActive: "#ffffff",
      sidebarBorder: "#e4e4e7",
      scrollbarThumb: "rgb(217 217 217)",
      scrollbarThumbHover: "rgb(191 191 191)",
    },
    {
      bgHex: "#000000",
      background: "#000000",
      sidebar: "#111111",
      card: "#111111",
      surfaceRaised: "#111111",
      popover: "#111111",
      foreground: "#f5f5f5",
      mutedForeground: "#a3a3a3",
      primary: "oklch(0.571 0.21 264)",
      primaryForeground: "#ffffff",
      secondary: "#0b0b0b",
      secondaryForeground: "#f5f5f5",
      muted: "#0b0b0b",
      accent: "#161616",
      accentForeground: "#f5f5f5",
      border: "#1f1f1f",
      input: "#282828",
      messageSurface: "#262626",
      codeBackground: "#0f0f0f",
      sidebarForeground: "#f1f3f7",
      sidebarMutedForeground: "#a3a3a3",
      sidebarRowHover: "#1c1c1c",
      sidebarRowActive: "#252525",
      sidebarBorder: "#1f1f1f",
      scrollbarThumb: "rgb(255 255 255 / 36%)",
      scrollbarThumbHover: "rgb(255 255 255 / 45%)",
    },
  ),
  builtIn(
    "t3-chat",
    "theme.name.t3Chat",
    "t3",
    {
      bgHex: "#fbf6fb",
      background: "oklch(0.982446 0.010114 325.653)",
      sidebar: "oklch(0.928886 0.031178 322.592)",
      card: "oklch(0.988235 0.005049 325.615)",
      foreground: "oklch(0.325698 0.116116 325.037)",
      mutedForeground: "oklch(0.494754 0.190937 354.544)",
      primary: "oklch(0.591646 0.217985 0.584)",
      primaryForeground: "oklch(1 0 0)",
      border: "oklch(0.923531 0.021247 328.096)",
      input: "oklch(0.851713 0.055822 336.6)",
      messageSurface: "oklch(0.926746 0.037898 332.6)",
      codeBackground: "oklch(0.953855 0.019695 315.668)",
      sidebarRowHover: "oklch(0.978851 0.001321 106.424)",
      sidebarRowActive: "oklch(0.978851 0.001321 106.424)",
      sidebarBorder: "oklch(0.938313 0.002552 48.717)",
    },
    {
      bgHex: "#1d1822",
      background: "oklch(0.22813 0.020366 307.469)",
      sidebar: "oklch(0.185778 0.019368 322.159)",
      card: "oklch(0.267101 0.02016 311.799)",
      surfaceRaised: "oklch(0.279864 0.021572 309.532)",
      foreground: "oklch(0.980735 0.004092 301.426)",
      mutedForeground: "oklch(0.880303 0.03077 342.696)",
      primary: "oklch(0.591646 0.217985 0.584)",
      primaryForeground: "oklch(0.98 0.01 343)",
      border: "oklch(0.325 0.02 305)",
      input: "oklch(0.345 0.035 344)",
      messageSurface: "oklch(0.273791 0.025541 309.079)",
      codeBackground: "oklch(0.22813 0.020366 307.469)",
      sidebarRowHover: "oklch(0.23366 0.026081 338.196)",
      sidebarRowActive: "oklch(0.256 0.032 338)",
      sidebarBorder: "oklch(0.269132 0.030766 351.067)",
    },
  ),
  builtIn(
    "grove",
    "theme.name.grove",
    "t3",
    {
      bgHex: "#f4f7f5",
      background: "oklch(0.972369 0.005497 157.15)",
      sidebar: "oklch(0.936464 0.014601 163.554)",
      card: "oklch(0.985 0.003 157)",
      surfaceRaised: "oklch(0.949276 0.004496 159.002)",
      foreground: T3_LIGHT_TEXT,
      mutedForeground: "oklch(0.527266 0.012309 320.683)",
      primary: "oklch(0.523295 0.112292 158.089)",
      primaryForeground: "oklch(0.990339 0.008411 325.64)",
      border: "oklch(0.864831 0.01312 167.255)",
      input: "oklch(0.829746 0.016084 168.234)",
      messageSurface: "oklch(0.891377 0.026164 164.929)",
      codeBackground: "oklch(0.955888 0.004783 158.391)",
      sidebarRowHover: "oklch(0.886676 0.027374 164.983)",
      sidebarRowActive: "oklch(0.85335 0.03597 165.158)",
      sidebarBorder: "oklch(0.860274 0.010287 168.339)",
    },
    {
      bgHex: "#1c2621",
      background: "oklch(0.260865 0.02152 162.75)",
      sidebar: "oklch(0.309925 0.032827 160.944)",
      card: "oklch(0.312979 0.018942 164.082)",
      surfaceRaised: "oklch(0.363192 0.016572 165.32)",
      foreground: T3_DARK_TEXT,
      mutedForeground: "oklch(0.715427 0.010896 171.428)",
      primary: "oklch(0.796228 0.133058 157.319)",
      primaryForeground: T3_LIGHT_TEXT,
      border: "oklch(0.457475 0.044046 160.971)",
      input: "oklch(0.519849 0.049896 160.863)",
      messageSurface: "oklch(0.41688 0.056069 159.165)",
      codeBackground: "oklch(0.312979 0.018942 164.082)",
      sidebarRowHover: "oklch(0.374959 0.047124 159.686)",
      sidebarRowActive: "oklch(0.41688 0.056069 159.165)",
      sidebarBorder: "oklch(0.457475 0.044046 160.971)",
    },
  ),
  builtIn(
    "ocean",
    "theme.name.ocean",
    "t3",
    {
      bgHex: "#f5f7fa",
      background: "oklch(0.974199 0.002856 241.597)",
      sidebar: "oklch(0.939254 0.01193 241.729)",
      card: "oklch(0.985 0.002 242)",
      surfaceRaised: "oklch(0.951058 0.002962 258.339)",
      foreground: T3_LIGHT_TEXT,
      mutedForeground: "oklch(0.528741 0.01828 313.823)",
      primary: "oklch(0.536684 0.120219 247.01)",
      primaryForeground: "oklch(0.990339 0.008411 325.64)",
      border: "oklch(0.867646 0.013482 252.362)",
      input: "oklch(0.832939 0.017389 252.598)",
      messageSurface: "oklch(0.895373 0.023469 241.913)",
      codeBackground: "oklch(0.957684 0.002906 253.68)",
      sidebarRowHover: "oklch(0.890798 0.024681 241.933)",
      sidebarRowActive: "oklch(0.858363 0.033325 242.089)",
      sidebarBorder: "oklch(0.862823 0.011384 256.926)",
    },
    {
      bgHex: "#18212b",
      background: "oklch(0.242641 0.024125 250.573)",
      sidebar: "oklch(0.290387 0.032043 247.274)",
      card: "oklch(0.29661 0.021883 251.968)",
      surfaceRaised: "oklch(0.348439 0.019942 253.696)",
      foreground: T3_DARK_TEXT,
      mutedForeground: "oklch(0.691936 0.016294 261.588)",
      primary: "oklch(0.758933 0.105833 241.548)",
      primaryForeground: T3_LIGHT_TEXT,
      border: "oklch(0.438653 0.039496 245.44)",
      input: "oklch(0.500905 0.043574 244.781)",
      messageSurface: "oklch(0.393878 0.048778 244.179)",
      codeBackground: "oklch(0.29661 0.021883 251.968)",
      sidebarRowHover: "oklch(0.353381 0.042285 245.043)",
      sidebarRowActive: "oklch(0.393878 0.048778 244.179)",
      sidebarBorder: "oklch(0.438653 0.039496 245.44)",
    },
  ),
  builtIn(
    "ember",
    "theme.name.ember",
    "t3",
    {
      bgHex: "#f9f6f3",
      background: "oklch(0.976527 0.002685 60.725)",
      sidebar: "oklch(0.942267 0.01151 50.785)",
      card: "oklch(0.988 0.002 60)",
      surfaceRaised: "oklch(0.953321 0.002701 42.266)",
      foreground: T3_LIGHT_TEXT,
      mutedForeground: "oklch(0.530413 0.018453 341.181)",
      primary: "oklch(0.552831 0.129438 44.656)",
      primaryForeground: "oklch(0.990339 0.008411 325.64)",
      border: "oklch(0.870631 0.013204 39.431)",
      input: "oklch(0.836213 0.017153 38.661)",
      messageSurface: "oklch(0.899296 0.022939 49.163)",
      codeBackground: "oklch(0.959965 0.002668 47.512)",
      sidebarRowHover: "oklch(0.894819 0.024151 49.073)",
      sidebarRowActive: "oklch(0.863104 0.032855 48.586)",
      sidebarBorder: "oklch(0.865593 0.011154 35.246)",
    },
    {
      bgHex: "#241d18",
      background: "oklch(0.245899 0.019144 42.044)",
      sidebar: "oklch(0.293349 0.029554 46.882)",
      card: "oklch(0.299662 0.017229 39.973)",
      surfaceRaised: "oklch(0.351262 0.01565 37.592)",
      foreground: T3_DARK_TEXT,
      mutedForeground: "oklch(0.692479 0.015227 30.963)",
      primary: "oklch(0.762174 0.124117 52.082)",
      primaryForeground: T3_LIGHT_TEXT,
      border: "oklch(0.44099 0.040202 48.807)",
      input: "oklch(0.503003 0.045721 49.44)",
      messageSurface: "oklch(0.396617 0.051353 50.201)",
      codeBackground: "oklch(0.299662 0.017229 39.973)",
      sidebarRowHover: "oklch(0.356163 0.042933 49.385)",
      sidebarRowActive: "oklch(0.396617 0.051353 50.201)",
      sidebarBorder: "oklch(0.44099 0.040202 48.807)",
    },
  ),
  builtIn(
    "iris",
    "theme.name.iris",
    "t3",
    {
      bgHex: "#f7f6fa",
      background: "oklch(0.976531 0.003855 303.226)",
      sidebar: "oklch(0.941387 0.014687 300.474)",
      card: "oklch(0.988 0.003 303)",
      surfaceRaised: "oklch(0.953326 0.004536 307.676)",
      foreground: T3_LIGHT_TEXT,
      mutedForeground: "oklch(0.529955 0.022319 321.556)",
      primary: "oklch(0.525348 0.15373 294.176)",
      primaryForeground: "oklch(0.990339 0.008411 325.64)",
      border: "oklch(0.869608 0.018226 303.859)",
      input: "oklch(0.834773 0.023405 303.676)",
      messageSurface: "oklch(0.897143 0.028558 299.758)",
      codeBackground: "oklch(0.95997 0.004338 306.542)",
      sidebarRowHover: "oklch(0.892522 0.030022 299.704)",
      sidebarRowActive: "oklch(0.85971 0.040501 299.36)",
      sidebarBorder: "oklch(0.864805 0.015938 305.371)",
    },
    {
      bgHex: "#1c1829",
      background: "oklch(0.225975 0.031062 293.741)",
      sidebar: "oklch(0.266743 0.044689 294.138)",
      card: "oklch(0.281873 0.028308 295.193)",
      surfaceRaised: "oklch(0.335291 0.026008 296.394)",
      foreground: T3_DARK_TEXT,
      mutedForeground: "oklch(0.663321 0.025932 301.862)",
      primary: "oklch(0.671712 0.169136 293.929)",
      primaryForeground: T3_LIGHT_TEXT,
      border: "oklch(0.40874 0.058536 295.893)",
      input: "oklch(0.46756 0.065775 296.265)",
      messageSurface: "oklch(0.355677 0.073167 294.217)",
      codeBackground: "oklch(0.281873 0.028308 295.193)",
      sidebarRowHover: "oklch(0.320808 0.062152 294.23)",
      sidebarRowActive: "oklch(0.355677 0.073167 294.217)",
      sidebarBorder: "oklch(0.40874 0.058536 295.893)",
    },
  ),
];

export const DEFAULT_THEME_ID = "default";

export function findBuiltInTheme(id: string | undefined): BuiltInTheme | undefined {
  return BUILT_IN_THEMES.find((theme) => theme.id === id);
}

export type ResolvedTheme = { kind: "builtin"; theme: BuiltInTheme } | { kind: "custom"; theme: CustomTheme };

export function resolveTheme(id: string | undefined, custom: readonly CustomTheme[]): ResolvedTheme {
  const builtin = findBuiltInTheme(id);
  if (builtin) return { kind: "builtin", theme: builtin };
  const found = custom.find((theme) => theme.id === id);
  if (found) return { kind: "custom", theme: found };
  return { kind: "builtin", theme: findBuiltInTheme(DEFAULT_THEME_ID)! };
}

/** The mode a theme renders in: single-appearance custom themes always use their own. */
export function themeMode(resolved: ResolvedTheme, preferred: ThemeMode): ThemeMode {
  if (resolved.kind === "custom" && resolved.theme.appearance !== "both") return resolved.theme.appearance;
  return preferred;
}

export function customSeed(theme: CustomTheme, mode: ThemeMode): CustomThemeSeed {
  const seed = theme[mode] ?? theme[mode === "dark" ? "light" : "dark"];
  return seed ?? seedFromTokens(findBuiltInTheme(DEFAULT_THEME_ID)![mode]);
}

export function customTokens(theme: CustomTheme, mode: ThemeMode): ThemePaletteTokens {
  return deriveTokensFromSeed(customSeed(theme, mode), mode, theme.overrides?.[mode]);
}

export function themeTokens(resolved: ResolvedTheme, mode: ThemeMode): ThemePaletteTokens {
  return resolved.kind === "builtin" ? resolved.theme[mode] : customTokens(resolved.theme, mode);
}

export function seedFromTokens(tokens: ThemePaletteTokens): CustomThemeSeed {
  const hex = (color: string) => resolveColorHex(color) ?? color;
  return {
    background: hex(tokens.background),
    sidebar: hex(tokens.sidebar),
    primary: hex(tokens.primary),
    foreground: hex(tokens.foreground),
    card: hex(tokens.card),
    messageSurface: hex(tokens.messageSurface),
  };
}

function withoutSeedRoles(tokens: ThemePaletteTokens): Partial<ThemePaletteTokens> {
  const rest: Partial<ThemePaletteTokens> = { ...tokens };
  delete rest.bgHex;
  for (const role of SEED_ROLES) delete rest[role];
  return rest;
}

export function newThemeId() {
  return `custom-${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

/** A custom copy of a built-in theme that keeps every role of both variants. */
export function customFromBuiltIn(theme: BuiltInTheme, name: string): CustomTheme {
  return {
    id: newThemeId(),
    name,
    appearance: "both",
    light: seedFromTokens(theme.light),
    dark: seedFromTokens(theme.dark),
    overrides: { light: withoutSeedRoles(theme.light), dark: withoutSeedRoles(theme.dark) },
  };
}

/** Applies a seed colour change and drops overrides that were tuned for the previous seed. */
export function withSeedColor(theme: CustomTheme, mode: ThemeMode, role: SeedRole, color: string): CustomTheme {
  const seed = { ...customSeed(theme, mode), [role]: color };
  const overrides = theme.overrides?.[mode] ? { ...theme.overrides[mode] } : undefined;
  if (overrides) {
    delete overrides[role];
    for (const dependent of SEED_DEPENDENTS[role]) delete overrides[dependent];
  }
  return { ...theme, [mode]: seed, overrides: { ...theme.overrides, [mode]: overrides } };
}

let probe: CanvasRenderingContext2D | null | undefined;
const hexCache = new Map<string, string | null>();

/** Resolves any safe CSS colour to an opaque `#rrggbb` by painting it (blended over white). */
export function resolveColorHex(color: string): string | null {
  const value = color.trim();
  if (/^#[0-9a-f]{6}$/i.test(value)) return value.toLowerCase();
  const short = /^#([0-9a-f])([0-9a-f])([0-9a-f])$/i.exec(value);
  if (short) return `#${short[1]}${short[1]}${short[2]}${short[2]}${short[3]}${short[3]}`.toLowerCase();
  if (hexCache.has(value)) return hexCache.get(value)!;
  let result: string | null = null;
  if (isSafeColor(value) && typeof document !== "undefined") {
    probe ??= document.createElement("canvas").getContext("2d", { willReadFrequently: true });
    if (probe) {
      probe.clearRect(0, 0, 1, 1);
      probe.fillStyle = "#ffffff";
      probe.fillRect(0, 0, 1, 1);
      probe.fillStyle = "#010203";
      probe.fillStyle = value;
      const applied = probe.fillStyle;
      if (applied !== "#010203" || /^#010203$/i.test(value)) {
        probe.fillRect(0, 0, 1, 1);
        const [r, g, b] = probe.getImageData(0, 0, 1, 1).data;
        result = `#${[r, g, b].map((channel) => channel!.toString(16).padStart(2, "0")).join("")}`;
      }
    }
  }
  hexCache.set(value, result);
  return result;
}

const UNSAFE_COLOR = /(?:var|url|attr|env|expression|image|calc)\s*\(|[;{}<>"'\\@!]/i;

/** True for plain colour values. Rejects anything that could reach beyond a colour (variables,
 * URLs, declarations) even where `CSS.supports` would accept it. */
export function isSafeColor(value: unknown): value is string {
  if (typeof value !== "string") return false;
  const trimmed = value.trim();
  if (!trimmed || trimmed.length > 120 || UNSAFE_COLOR.test(trimmed)) return false;
  if (typeof CSS === "undefined" || typeof CSS.supports !== "function") return /^#[0-9a-f]{3,8}$/i.test(trimmed);
  return CSS.supports("color", trimmed);
}

export function exportThemeJson(theme: CustomTheme | BuiltInTheme): string {
  const custom = "nameKey" in theme ? customFromBuiltIn(theme, theme.id) : theme;
  const out: Record<string, unknown> = { version: 1, format: "verre-theme", name: custom.name, appearance: custom.appearance };
  if (custom.appearance !== "dark" && custom.light) out.light = custom.light;
  if (custom.appearance !== "light" && custom.dark) out.dark = custom.dark;
  const overrides: Partial<Record<ThemeMode, Partial<ThemePaletteTokens>>> = {};
  for (const mode of ["light", "dark"] as const) {
    if (custom.appearance !== "both" && custom.appearance !== mode) continue;
    const entries = Object.entries(custom.overrides?.[mode] ?? {}).filter(([key]) => key !== "bgHex");
    if (entries.length > 0) overrides[mode] = Object.fromEntries(entries);
  }
  if (Object.keys(overrides).length > 0) out.overrides = overrides;
  return JSON.stringify(out, null, 2);
}

const T3_KEYS: Record<string, ThemeColorRole> = {
  canvas: "background",
  background: "background",
  sidebar: "sidebar",
  surface: "card",
  card: "card",
  surfaceRaised: "surfaceRaised",
  surfaceOverlay: "popover",
  popover: "popover",
  text: "foreground",
  foreground: "foreground",
  textMuted: "mutedForeground",
  mutedForeground: "mutedForeground",
  accent: "primary",
  primary: "primary",
  accentForeground: "primaryForeground",
  primaryForeground: "primaryForeground",
  secondary: "secondary",
  secondaryForeground: "secondaryForeground",
  muted: "muted",
  hover: "accent",
  border: "border",
  input: "input",
  ring: "ring",
  focusRing: "ring",
  messageSurface: "messageSurface",
  messageForeground: "messageForeground",
  messageText: "messageForeground",
  codeBackground: "codeBackground",
  sidebarForeground: "sidebarForeground",
  sidebarMutedForeground: "sidebarMutedForeground",
  sidebarRowHover: "sidebarRowHover",
  sidebarRowActive: "sidebarRowActive",
  sidebarBorder: "sidebarBorder",
  danger: "error",
  destructive: "error",
  error: "error",
  warning: "warning",
  success: "success",
  info: "info",
};

const VERRE_ROLES = new Set<string>(COLOR_ROLES);

type ParseResult = { ok: true; theme: CustomTheme } | { ok: false; error: string };

const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value);

function readColors(source: unknown, keys: Record<string, ThemeColorRole> | null, invalid: string[]): Partial<ThemePaletteTokens> {
  const out: Partial<ThemePaletteTokens> = {};
  if (!isRecord(source)) return out;
  for (const [key, value] of Object.entries(source)) {
    const role = keys ? (keys[key] ?? (VERRE_ROLES.has(key) ? (key as ThemeColorRole) : undefined)) : VERRE_ROLES.has(key) ? (key as ThemeColorRole) : undefined;
    if (!role) continue;
    if (isSafeColor(value)) out[role] = value.trim();
    else invalid.push(key);
  }
  return out;
}

function seedFrom(tokens: Partial<ThemePaletteTokens>, mode: ThemeMode): CustomThemeSeed | null {
  if (!tokens.background || !tokens.foreground) return null;
  const full = completeTokens({ ...tokens, background: tokens.background, foreground: tokens.foreground, primary: tokens.primary ?? (mode === "dark" ? "#6b8cff" : "#2563eb") }, mode);
  const keep = (color: string) => (/^color-mix\(/.test(color) ? (resolveColorHex(color) ?? color) : color);
  return {
    background: full.background,
    sidebar: keep(full.sidebar),
    primary: full.primary,
    foreground: full.foreground,
    card: keep(full.card),
    messageSurface: keep(full.messageSurface),
  };
}

function overridesFrom(tokens: Partial<ThemePaletteTokens>): Partial<ThemePaletteTokens> | undefined {
  const rest: Partial<ThemePaletteTokens> = { ...tokens };
  for (const role of SEED_ROLES) delete rest[role];
  delete rest.bgHex;
  return Object.keys(rest).length > 0 ? rest : undefined;
}

function readSeed(source: unknown, invalid: string[], mode: ThemeMode): CustomThemeSeed | null {
  if (!isRecord(source)) return null;
  return seedFrom(readColors(source, null, invalid), mode);
}

function appearanceOf(value: unknown, hasLight: boolean, hasDark: boolean): ThemeAppearance {
  if (value === "light" || value === "dark" || value === "both") {
    if (value === "both" && !(hasLight && hasDark)) return hasLight ? "light" : "dark";
    if (value === "light" && !hasLight) return "dark";
    if (value === "dark" && !hasDark) return "light";
    return value;
  }
  if (hasLight && hasDark) return "both";
  return hasDark ? "dark" : "light";
}

function themeName(value: unknown) {
  return typeof value === "string" && value.trim() ? value.trim().slice(0, 60) : "Imported theme";
}

function parseT3(data: Record<string, unknown>, invalid: string[]): ParseResult {
  const base = readColors(data.colors, T3_KEYS, invalid);
  const variants = isRecord(data.variants) ? data.variants : {};
  const variant = (mode: ThemeMode) => {
    const entry = variants[mode];
    return readColors(isRecord(entry) && isRecord(entry.colors) ? entry.colors : entry, T3_KEYS, invalid);
  };
  const declared = data.appearance;
  const tokens: Partial<Record<ThemeMode, Partial<ThemePaletteTokens>>> = {};
  for (const mode of ["light", "dark"] as const) {
    const own = variant(mode);
    const baseApplies = declared === mode || declared === "both" || declared === undefined || declared === "system" || Object.keys(own).length > 0;
    const merged = own.background ? own : { ...(baseApplies ? base : {}), ...own };
    if (merged.background && merged.foreground) tokens[mode] = merged;
  }
  if (declared === "light" && !variants.dark) delete tokens.dark;
  if (declared === "dark" && !variants.light) delete tokens.light;
  if (declared === undefined && !variants.light && !variants.dark && tokens.light && tokens.dark) {
    const lightBg = resolveColorHex(tokens.light.background!);
    const l = lightBg ? luminance(lightBg) : null;
    if (l !== null && l < 0.2) delete tokens.light;
    else delete tokens.dark;
  }
  const light = tokens.light ? seedFrom(tokens.light, "light") : null;
  const dark = tokens.dark ? seedFrom(tokens.dark, "dark") : null;
  if (!light && !dark) return { ok: false, error: "missing" };
  const overrides: CustomTheme["overrides"] = {};
  if (tokens.light && light) overrides.light = overridesFrom(tokens.light);
  if (tokens.dark && dark) overrides.dark = overridesFrom(tokens.dark);
  return {
    ok: true,
    theme: {
      id: newThemeId(),
      name: themeName(data.name),
      appearance: appearanceOf(declared, !!light, !!dark),
      ...(light ? { light } : {}),
      ...(dark ? { dark } : {}),
      overrides,
    },
  };
}

function parseVerre(data: Record<string, unknown>, invalid: string[]): ParseResult {
  const shared = readSeed(data.seed, invalid, data.appearance === "dark" ? "dark" : "light");
  let light = readSeed(data.light, invalid, "light");
  let dark = readSeed(data.dark, invalid, "dark");
  if (shared) {
    if (data.appearance === "dark") dark ??= shared;
    else light ??= shared;
  }
  if (!light && !dark) return { ok: false, error: "missing" };
  const overrides: CustomTheme["overrides"] = {};
  if (isRecord(data.overrides)) {
    for (const mode of ["light", "dark"] as const) {
      const read = readColors(data.overrides[mode], null, invalid);
      delete read.bgHex;
      if (Object.keys(read).length > 0) overrides[mode] = read;
    }
  }
  return {
    ok: true,
    theme: {
      id: newThemeId(),
      name: themeName(data.name),
      appearance: appearanceOf(data.appearance, !!light, !!dark),
      ...(light ? { light } : {}),
      ...(dark ? { dark } : {}),
      overrides,
    },
  };
}

/** Parses Verre theme JSON or a T3 Code ThemeFile v1. Error strings are codes for the UI to
 * translate: `json`, `shape`, `version`, `missing`, `invalid:<keys>`. Raw `css` is ignored. */
export function parseThemeJson(raw: string): ParseResult {
  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch {
    return { ok: false, error: "json" };
  }
  if (!isRecord(data)) return { ok: false, error: "shape" };
  if (data.version !== undefined && data.version !== 1) return { ok: false, error: "version" };
  const invalid: string[] = [];
  const result = "colors" in data || "variants" in data ? parseT3(data, invalid) : parseVerre(data, invalid);
  if (!result.ok && invalid.length > 0) return { ok: false, error: `invalid:${[...new Set(invalid)].slice(0, 6).join(", ")}` };
  return result;
}

/** Normalises a stored custom theme, dropping anything that is not a safe colour. */
export function sanitizeCustomTheme(value: unknown): CustomTheme | null {
  if (!isRecord(value) || typeof value.id !== "string" || !value.id) return null;
  const invalid: string[] = [];
  const light = readSeed(value.light, invalid, "light");
  const dark = readSeed(value.dark, invalid, "dark");
  if (!light && !dark) return null;
  const overrides: CustomTheme["overrides"] = {};
  if (isRecord(value.overrides)) {
    for (const mode of ["light", "dark"] as const) {
      const read = readColors(value.overrides[mode], null, invalid);
      delete read.bgHex;
      if (Object.keys(read).length > 0) overrides[mode] = read;
    }
  }
  return {
    id: value.id,
    name: themeName(value.name),
    appearance: appearanceOf(value.appearance, !!light, !!dark),
    ...(light ? { light } : {}),
    ...(dark ? { dark } : {}),
    overrides,
  };
}

/** Writes a palette onto the root element; `null` restores the stylesheet defaults. */
export function writeTokens(root: HTMLElement, tokens: ThemePaletteTokens | null) {
  for (const name of MANAGED_VARS) root.style.removeProperty(name);
  if (!tokens) return;
  for (const role of COLOR_ROLES) {
    const value = tokens[role];
    if (!value) continue;
    for (const name of TOKEN_VARS[role]) root.style.setProperty(name, value);
  }
}
