import { useCallback } from "react";
import { resolvedLanguage, usePrefs } from "~/state/prefs";
import { en, type MessageKey } from "./en";
import { zh } from "./zh";

const dictionaries = { en, zh } as const;

export type Translate = (key: MessageKey, vars?: Record<string, string | number>) => string;

export function translate(lang: "en" | "zh", key: MessageKey, vars?: Record<string, string | number>) {
  const template: string = dictionaries[lang][key] ?? en[key];
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (match, name: string) => (name in vars ? String(vars[name]) : match));
}

export function useLang() {
  return resolvedLanguage(usePrefs((state) => state.language));
}

export function useT(): Translate {
  const lang = useLang();
  return useCallback((key, vars) => translate(lang, key, vars), [lang]);
}

export type { MessageKey };
