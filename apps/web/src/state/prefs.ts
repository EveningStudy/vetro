import { create } from "zustand";

export type ThemePref = "light" | "dark" | "system";
export type LanguagePref = "en" | "zh" | "system";

const THEME_KEY = "vetro:theme";
const LANGUAGE_KEY = "vetro:language";

const query = new URLSearchParams(location.search);

function readPref<T extends string>(key: string, allowed: readonly T[], fallback: T, override: string | null): T {
  const value = override ?? localStorage.getItem(key);
  return allowed.includes(value as T) ? (value as T) : fallback;
}

export const usePrefs = create<{ theme: ThemePref; language: LanguagePref }>(() => ({
  theme: readPref(THEME_KEY, ["light", "dark", "system"] as const, "system", query.get("theme")),
  language: readPref(LANGUAGE_KEY, ["en", "zh", "system"] as const, "system", query.get("lang")),
}));

const media = matchMedia("(prefers-color-scheme: dark)");

export function applyTheme() {
  const { theme } = usePrefs.getState();
  const dark = theme === "dark" || (theme === "system" && media.matches);
  const root = document.documentElement;
  root.classList.add("no-transitions");
  root.classList.toggle("dark", dark);
  root.style.backgroundColor = "";
  requestAnimationFrame(() => root.classList.remove("no-transitions"));
}

media.addEventListener("change", applyTheme);

export function setTheme(theme: ThemePref) {
  localStorage.setItem(THEME_KEY, theme);
  usePrefs.setState({ theme });
  applyTheme();
}

export function setLanguage(language: LanguagePref) {
  localStorage.setItem(LANGUAGE_KEY, language);
  usePrefs.setState({ language });
}

export function resolvedLanguage(pref: LanguagePref): "en" | "zh" {
  if (pref !== "system") return pref;
  return navigator.languages.some((lang) => lang.toLowerCase().startsWith("zh")) ? "zh" : "en";
}
