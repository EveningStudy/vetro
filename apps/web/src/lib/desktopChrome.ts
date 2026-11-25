import { useEffect, useSyncExternalStore } from "react";

export type DesktopPlatform = "windows" | "macos" | "linux" | "web";
/** `custom`: frameless window, Vetro draws the caption buttons. `overlay`: native traffic lights
 * over the web content. `native`: the OS title bar. `web`: a browser or a paired device. */
export type DesktopChromeMode = "custom" | "overlay" | "native" | "web";

export interface DesktopChromeState {
  platform: DesktopPlatform;
  chrome: DesktopChromeMode;
  maximized: boolean;
}

declare global {
  interface Window {
    __VETRO_DESKTOP__?: { platform?: string; chrome?: string; maximized?: boolean };
    __TAURI_INTERNALS__?: {
      invoke?: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
      transformCallback?: (callback: (event: { payload: unknown }) => void, once?: boolean) => number;
    };
  }
}

const WEB: DesktopChromeState = { platform: "web", chrome: "web", maximized: false };

function detect(): DesktopChromeState {
  const injected = window.__VETRO_DESKTOP__;
  if (injected) {
    const platform = injected.platform === "windows" || injected.platform === "macos" ? injected.platform : "linux";
    const chrome = injected.chrome === "custom" || injected.chrome === "overlay" ? injected.chrome : "native";
    return { platform, chrome, maximized: injected.maximized === true };
  }
  const params = new URLSearchParams(location.search);
  if (import.meta.env.DEV || import.meta.env.VITE_MOCK === "1" || params.get("mock") === "1") {
    const preview = params.get("chrome");
    if (preview === "windows") return { platform: "windows", chrome: "custom", maximized: false };
    if (preview === "macos") return { platform: "macos", chrome: "overlay", maximized: false };
  }
  return WEB;
}

let state = detect();
const listeners = new Set<() => void>();

function setMaximized(maximized: boolean) {
  if (state.maximized === maximized) return;
  state = { ...state, maximized };
  for (const listener of listeners) listener();
}

window.addEventListener("vetro:window-state", (event) => {
  const detail = (event as CustomEvent<{ maximized?: unknown }>).detail;
  if (typeof detail?.maximized === "boolean") setMaximized(detail.maximized);
});

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function getDesktopChrome(): DesktopChromeState {
  return state;
}

async function invokeWindow(cmd: string): Promise<unknown> {
  if (state.chrome === "web") return undefined;
  const invoke = window.__TAURI_INTERNALS__?.invoke;
  if (!invoke) return undefined;
  try {
    return await invoke(`plugin:window|${cmd}`, { label: "main" });
  } catch {
    return undefined;
  }
}

export const startWindowDragging = () => invokeWindow("start_dragging");
export const minimizeWindow = () => invokeWindow("minimize");
export const toggleMaximizeWindow = () => invokeWindow("toggle_maximize");
export const closeWindow = () => invokeWindow("close");

export async function pickSystemFolder(defaultPath?: string): Promise<string | null> {
  if (state.chrome === "web") return null;
  const invoke = window.__TAURI_INTERNALS__?.invoke;
  if (!invoke) return null;
  try {
    const result = await invoke("plugin:dialog|open", {
      options: {
        directory: true,
        multiple: false,
        ...(defaultPath ? { defaultPath } : {}),
      },
    });
    if (typeof result === "string" && result.trim()) return result;
    if (Array.isArray(result) && typeof result[0] === "string" && result[0].trim()) return result[0];
    return null;
  } catch {
    return null;
  }
}

/** Absolute paths chosen in the system file dialog. Empty outside the desktop shell. */
export async function pickSystemFiles(): Promise<string[]> {
  if (state.chrome === "web") return [];
  const invoke = window.__TAURI_INTERNALS__?.invoke;
  if (!invoke) return [];
  try {
    const result = await invoke("plugin:dialog|open", { options: { multiple: true } });
    const paths = Array.isArray(result) ? result : [result];
    return paths.filter((path): path is string => typeof path === "string" && path.trim() !== "");
  } catch {
    return [];
  }
}

export type FileDropEvent = { kind: "over" } | { kind: "leave" } | { kind: "drop"; paths: string[] };

/**
 * The desktop webview handles file drops natively and reports absolute paths; HTML drop
 * events carry no paths there. Returns an unsubscribe function, or null outside the shell.
 */
export function listenFileDrops(handler: (event: FileDropEvent) => void): (() => void) | null {
  if (state.chrome === "web") return null;
  const internals = window.__TAURI_INTERNALS__;
  const invoke = internals?.invoke;
  const transform = internals?.transformCallback;
  if (!invoke || !transform) return null;
  const subscriptions = (
    [
      ["tauri://drag-enter", () => handler({ kind: "over" })],
      ["tauri://drag-over", () => handler({ kind: "over" })],
      ["tauri://drag-leave", () => handler({ kind: "leave" })],
      [
        "tauri://drag-drop",
        (payload: unknown) => {
          const paths = (payload as { paths?: unknown } | null)?.paths;
          handler({ kind: "drop", paths: Array.isArray(paths) ? paths.filter((path): path is string => typeof path === "string") : [] });
        },
      ],
    ] as const
  ).map(([event, callback]) => {
    const id = transform((message) => callback(message.payload));
    return invoke("plugin:event|listen", { event, target: { kind: "Any" }, handler: id })
      .then((eventId) => ({ event, eventId }))
      .catch(() => null);
  });
  return () => {
    for (const subscription of subscriptions) {
      void subscription.then((active) => {
        if (active) void invoke("plugin:event|unlisten", { event: active.event, eventId: active.eventId }).catch(() => {});
      });
    }
  };
}

export async function queryWindowMaximized(): Promise<boolean | undefined> {
  const result = await invokeWindow("is_maximized");
  return typeof result === "boolean" ? result : undefined;
}

export function useDesktopChrome(): DesktopChromeState {
  const current = useSyncExternalStore(subscribe, getDesktopChrome);
  const custom = current.chrome === "custom";
  useEffect(() => {
    if (!custom) return;
    void queryWindowMaximized().then((maximized) => {
      if (maximized !== undefined) setMaximized(maximized);
    });
  }, [custom]);
  return current;
}
