import type * as React from "react";
import { useT } from "~/i18n";
import { closeWindow, minimizeWindow, startWindowDragging, toggleMaximizeWindow, useDesktopChrome } from "~/lib/desktopChrome";
import { cn } from "~/lib/utils";
import { usePrefs } from "~/state/prefs";
import { useStore } from "~/state/store";
import { useMediaQuery } from "./hooks";

const INTERACTIVE = 'button, a, input, textarea, select, [role="button"], [role="menuitem"], [role="combobox"], [data-no-drag]';

const captionClass =
  "inline-flex h-full w-[46px] items-center justify-center text-foreground/80 outline-none transition-colors duration-100 focus-visible:bg-foreground/[0.08] [&_svg]:pointer-events-none";

/** Windows caption buttons for the frameless desktop window. Renders nothing anywhere else. */
export function WindowControls() {
  const t = useT();
  const { chrome, maximized } = useDesktopChrome();
  if (chrome !== "custom") return null;
  return (
    <div className="fixed top-0 right-0 z-[65] flex h-(--workspace-topbar-height) items-stretch select-none" data-no-drag>
      <span aria-hidden className="my-auto me-0.5 h-4 w-px bg-border/80" />
      <button type="button" tabIndex={-1} aria-label={t("window.minimize")} title={t("window.minimize")} onClick={() => void minimizeWindow()} className={cn(captionClass, "hover:bg-foreground/[0.08] active:bg-foreground/[0.12]")}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden shapeRendering="crispEdges">
          <path d="M0 5.5h10" stroke="currentColor" strokeWidth="1" />
        </svg>
      </button>
      <button
        type="button"
        tabIndex={-1}
        aria-label={maximized ? t("window.restore") : t("window.maximize")}
        title={maximized ? t("window.restore") : t("window.maximize")}
        onClick={() => void toggleMaximizeWindow()}
        className={cn(captionClass, "hover:bg-foreground/[0.08] active:bg-foreground/[0.12]")}
      >
        {maximized ? (
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden shapeRendering="crispEdges" fill="none" stroke="currentColor" strokeWidth="1">
            <rect x="0.5" y="2.5" width="7" height="7" />
            <path d="M2.5 2.5V0.5h7v7h-2" />
          </svg>
        ) : (
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden shapeRendering="crispEdges" fill="none" stroke="currentColor" strokeWidth="1">
            <rect x="0.5" y="0.5" width="9" height="9" />
          </svg>
        )}
      </button>
      <button type="button" tabIndex={-1} aria-label={t("window.close")} title={t("window.close")} onClick={() => void closeWindow()} className={cn(captionClass, "hover:bg-[#e81123] hover:text-white active:bg-[#f1707a] active:text-white")}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden fill="none" stroke="currentColor" strokeWidth="1" strokeLinecap="square">
          <path d="M0.5 0.5l9 9M9.5 0.5l-9 9" />
        </svg>
      </button>
    </div>
  );
}

function interactive(target: EventTarget | null) {
  return target instanceof Element && target.closest(INTERACTIVE) !== null;
}

type DragProps = Pick<React.HTMLAttributes<HTMLElement>, "onMouseDown" | "onDoubleClick"> & { "data-window-drag-region"?: "" };

/** Lets empty parts of a top bar move the window (and double-click maximise it) on desktop shells
 * that draw their own title bar. Clicks on controls inside the bar are left alone.
 *
 * Deliberately not `data-tauri-drag-region`: Tauri's built-in drag script would handle the same
 * mouse events, and a second toggle_maximize on double-click undoes the first. */
export function useWindowDragProps(): DragProps {
  const { chrome } = useDesktopChrome();
  if (chrome !== "custom" && chrome !== "overlay") return {};
  return {
    "data-window-drag-region": "",
    onMouseDown: (event) => {
      if (event.button !== 0 || event.detail !== 1 || interactive(event.target)) return;
      void startWindowDragging();
    },
    onDoubleClick: (event) => {
      if (event.button !== 0 || interactive(event.target)) return;
      void toggleMaximizeWindow();
    },
  };
}

const TRAFFIC_LIGHTS = "ps-[78px]";
const CAPTION_BUTTONS = "pe-[152px]";

/** Padding that keeps a top bar's own controls clear of the traffic lights or caption buttons.
 * `main` is the thread header beside an optional right drawer; `main-full` spans to the window edge. */
export function useHeaderChromeClass(slot: "sidebar" | "main" | "main-full" | "drawer"): string {
  const { chrome } = useDesktopChrome();
  const collapsed = usePrefs((s) => s.sidebarCollapsed);
  const drawerOpen = useStore((s) => Boolean(s.rightDrawer));
  const desktop = useMediaQuery("(min-width: 48rem)");
  if (chrome === "overlay") {
    if (slot === "sidebar") return TRAFFIC_LIGHTS;
    if (slot === "main" || slot === "main-full") return collapsed ? TRAFFIC_LIGHTS : "max-md:ps-[78px]";
    return "";
  }
  if (chrome !== "custom" || slot === "sidebar") return "";
  if (slot === "main" && drawerOpen && desktop) return "";
  return CAPTION_BUTTONS;
}

/** True when the window has no native title bar, so every view needs a draggable top strip. */
export function useIntegratedTitleBar() {
  const { chrome } = useDesktopChrome();
  return chrome === "custom" || chrome === "overlay";
}
