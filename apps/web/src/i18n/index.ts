import { useCallback, useEffect, useSyncExternalStore } from "react";
import {
  htmlLangTag,
  LOCALE_LABELS,
  LOCALES,
  type Locale,
  normalizeLocale,
  resolvedLanguage,
  usePrefs,
} from "~/state/prefs";
import { en, type MessageKey } from "./en";

export { htmlLangTag, LOCALE_LABELS, LOCALES, normalizeLocale, type Locale };
export type Translate = (key: MessageKey, vars?: Record<string, string | number>) => string;

type Dictionary = Record<MessageKey, string>;

const loaders: Record<Exclude<Locale, "en">, () => Promise<Dictionary>> = {
  zh: () => import("./zh").then((m) => m.zh),
  "zh-Hant": () => import("./zh-Hant").then((m) => m.zhHant),
  ja: () => import("./ja").then((m) => m.ja),
  ko: () => import("./ko").then((m) => m.ko),
  es: () => import("./es").then((m) => m.es),
  de: () => import("./de").then((m) => m.de),
  fr: () => import("./fr").then((m) => m.fr),
};

const loaded: Partial<Record<Locale, Dictionary>> = { en };
const inflight = new Map<Locale, Promise<void>>();
const listeners = new Set<() => void>();
let version = 0;

function notify() {
  version += 1;
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function snapshot() {
  return version;
}

export async function ensureLocale(lang: Locale): Promise<void> {
  if (loaded[lang]) return;
  const existing = inflight.get(lang);
  if (existing) return existing;
  const task = loaders[lang as Exclude<Locale, "en">]()
    .then((dict) => {
      loaded[lang] = dict;
      notify();
    })
    .catch(() => {
      // Fall back to English strings if a lazy locale chunk fails to load.
      notify();
    })
    .finally(() => {
      inflight.delete(lang);
    });
  inflight.set(lang, task);
  return task;
}

usePrefs.subscribe((state, prev) => {
  if (state.language !== prev.language) {
    void ensureLocale(resolvedLanguage(state.language));
  }
});

if (typeof window !== "undefined") {
  window.addEventListener("online", () => {
    const active = resolvedLanguage(usePrefs.getState().language);
    if (!loaded[active]) void ensureLocale(active);
  });
}

export function loadedTranslations(key: MessageKey): string[] {
  const out = new Set<string>([en[key]]);
  for (const lang of LOCALES) {
    const dict = loaded[lang];
    if (dict) out.add(dict[key]);
  }
  return [...out];
}

export function translate(lang: Locale, key: MessageKey, vars?: Record<string, string | number>) {
  const dict = loaded[lang];
  if (!dict && lang !== "en") void ensureLocale(lang);
  const template: string = dict?.[key] ?? en[key];
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (match, name: string) => (name in vars ? String(vars[name]) : match));
}

export function useLang(): Locale {
  return resolvedLanguage(usePrefs((state) => state.language));
}

export function useDisplayLang(): Locale {
  const lang = useLang();
  useSyncExternalStore(subscribe, snapshot, snapshot);
  return loaded[lang] ? lang : "en";
}

export function useT(): Translate {
  const lang = useLang();
  const dictVersion = useSyncExternalStore(subscribe, snapshot, snapshot);
  useEffect(() => {
    void ensureLocale(lang);
  }, [lang]);
  return useCallback(
    (key, vars) => {
      void dictVersion;
      return translate(lang, key, vars);
    },
    [lang, dictVersion],
  );
}

export type { MessageKey };
