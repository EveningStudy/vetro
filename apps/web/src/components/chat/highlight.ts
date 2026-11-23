import { useSyncExternalStore } from "react";

type Highlight = (code: string, lang: string | undefined) => string | null;

let loaded: Highlight | null = null;
let loading: Promise<void> | null = null;
const listeners = new Set<() => void>();

function load() {
  loading ??= import("./highlighter").then((module) => {
    loaded = module.highlightCode;
    for (const listener of listeners) listener();
  });
  return loading;
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  void load();
  return () => {
    listeners.delete(listener);
  };
}

/** highlight.js is loaded on first use, so the first paint does not wait for it. */
export function useHighlighter(): Highlight | null {
  return useSyncExternalStore(subscribe, () => loaded);
}
