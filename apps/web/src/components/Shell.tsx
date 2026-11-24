import { Dialog as DialogPrimitive } from "@base-ui/react/dialog";
import { CircleAlertIcon, CircleCheckIcon, InfoIcon, PanelLeftIcon, PanelLeftOpenIcon, WifiOffIcon, XIcon } from "lucide-react";
import { useEffect } from "react";
import { useT } from "~/i18n";
import { cn, isMac } from "~/lib/utils";
import { usePrefs } from "~/state/prefs";
import { actions, useStore } from "~/state/store";
import { dismiss, useToasts } from "~/state/toast";
import { CommandPalette } from "./CommandPalette";
import { EmptyState } from "./EmptyState";
import { SettingsView } from "./settings/SettingsView";
import { NewThread } from "./chat/NewThread";
import { ThreadView } from "./chat/ThreadView";
import { SidebarContent } from "./sidebar/Sidebar";
import { Button } from "./ui/button";
import { Tip } from "./ui/overlay";
import { WindowControls } from "./WindowChrome";

/** Whether page headers need to show on desktop to hold the expand-sidebar button. */
export function useSidebarCollapsed() {
  return usePrefs((s) => s.sidebarCollapsed);
}

/** Opens the drawer sidebar on small screens and, when the desktop sidebar is collapsed, expands it. */
export function MobileSidebarButton({ className }: { className?: string }) {
  const t = useT();
  const collapsed = useSidebarCollapsed();
  return (
    <>
      <Button
        size="icon-sm"
        variant="ghost"
        className={cn("-ms-1.5 md:hidden", className)}
        aria-label={t("sidebar.toggle")}
        onClick={() => useStore.setState({ mobileSidebarOpen: true })}
      >
        <PanelLeftIcon />
      </Button>
      {collapsed ? (
        <Tip label={`${t("sidebar.expand")} · ${isMac ? "⌘B" : "Ctrl B"}`} side="bottom">
          <Button size="icon-sm" variant="ghost" className={cn("-ms-1.5 max-md:hidden", className)} aria-label={t("sidebar.expand")} onClick={actions.toggleSidebar}>
            <PanelLeftOpenIcon />
          </Button>
        </Tip>
      ) : null}
    </>
  );
}

function MobileSidebar() {
  const open = useStore((s) => s.mobileSidebarOpen);
  return (
    <DialogPrimitive.Root open={open} onOpenChange={(next) => useStore.setState({ mobileSidebarOpen: next })}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Backdrop className="fixed inset-0 z-50 bg-black/40 transition-opacity duration-200 data-ending-style:opacity-0 data-starting-style:opacity-0 md:hidden" />
        <DialogPrimitive.Popup className="fixed inset-y-0 left-0 z-50 w-[min(20rem,calc(100vw-3rem))] border-r bg-sidebar pt-[env(safe-area-inset-top)] text-sidebar-foreground shadow-xl outline-none transition-transform duration-250 ease-(--ease-drawer) data-ending-style:-translate-x-full data-starting-style:-translate-x-full md:hidden">
          <SidebarContent />
        </DialogPrimitive.Popup>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

function ConnectionBanner() {
  const t = useT();
  const connection = useStore((s) => s.connection);
  const loaded = useStore((s) => s.threadsLoaded);
  if (connection === "open") return null;
  return (
    <div className="pointer-events-none fixed inset-x-0 top-[calc(env(safe-area-inset-top)+0.75rem)] z-[60] flex justify-center">
      <div className="dropdown-glass flex items-center gap-2 rounded-full px-3 py-1.5 text-xs font-medium shadow-lg">
        <WifiOffIcon className="size-3.5 text-warning-foreground" />
        {loaded ? t("conn.lost") : t("conn.connecting")}
      </div>
    </div>
  );
}

function Toasts() {
  const toasts = useToasts((s) => s.toasts);
  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-[calc(env(safe-area-inset-bottom)+1rem)] z-[150] flex flex-col items-center gap-2 px-4 sm:right-4 sm:left-auto sm:items-end">
      {toasts.map((toast) => {
        const Icon = toast.kind === "error" ? CircleAlertIcon : toast.kind === "success" ? CircleCheckIcon : InfoIcon;
        return (
          <div key={toast.id} className="dropdown-glass pointer-events-auto flex max-w-sm items-start gap-2 rounded-xl px-3 py-2.5 text-sm shadow-lg">
            <Icon className={cn("mt-0.5 size-4 shrink-0", toast.kind === "error" ? "text-error" : toast.kind === "success" ? "text-success" : "text-info")} />
            <span className="min-w-0 flex-1">{toast.text}</span>
            <button type="button" onClick={() => dismiss(toast.id)} className="shrink-0 text-muted-foreground hover:text-foreground">
              <XIcon className="size-3.5" />
            </button>
          </div>
        );
      })}
    </div>
  );
}

function overlayOpen() {
  return document.querySelector('[role="dialog"], [role="alertdialog"], [role="menu"], [role="listbox"]') !== null;
}

export function Shell() {
  const route = useStore((s) => s.route);
  const collapsed = useSidebarCollapsed();

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.isComposing || event.keyCode === 229) return;
      const state = useStore.getState();
      if ((event.metaKey || event.ctrlKey) && !event.altKey) {
        const key = event.key.toLowerCase();
        if (key === "k") {
          event.preventDefault();
          useStore.setState((current) => ({ paletteOpen: !current.paletteOpen }));
          return;
        }
        if (!event.shiftKey && key === "b") {
          event.preventDefault();
          actions.toggleSidebar();
          return;
        }
        if (!event.shiftKey && key === "n") {
          event.preventDefault();
          const summary = state.route.view === "thread" ? (state.threads[state.route.threadId] ?? state.open[state.route.threadId]?.summary) : undefined;
          const cwd = summary?.cwd?.trim() ? summary.cwd : null;
          actions.navigate({ view: "new", projectPath: cwd, agentId: null });
          return;
        }
        if (event.shiftKey && state.route.view === "thread" && (key === "d" || key === "l")) {
          event.preventDefault();
          if (key === "d") actions.toggleChangesDrawer();
          else actions.toggleRawDrawer();
          return;
        }
        return;
      }
      if (event.altKey && !event.metaKey && !event.ctrlKey && !event.shiftKey && !overlayOpen() && !(event.target instanceof HTMLSelectElement)) {
        if (event.key === "ArrowUp" || event.key === "ArrowDown") {
          event.preventDefault();
          actions.cycleThread(event.key === "ArrowDown" ? 1 : -1);
          return;
        }
        const digit = /^Digit([1-9])$/.exec(event.code)?.[1];
        if (digit) {
          event.preventDefault();
          actions.jumpToActive(Number(digit) - 1);
          return;
        }
      }
      if (event.key === "Escape" && !event.defaultPrevented && state.rightDrawer && !state.paletteOpen && !state.mobileSidebarOpen && !overlayOpen()) {
        actions.closeDrawer();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="flex h-dvh min-h-0 w-full">
      <aside
        data-app-sidebar
        data-collapsed={collapsed || undefined}
        inert={collapsed}
        className={cn(
          "hidden h-full shrink-0 overflow-hidden border-sidebar-border bg-sidebar text-sidebar-foreground transition-[width,border-color] duration-200 ease-out md:block",
          collapsed ? "w-0 border-r-0" : "w-64 border-r",
        )}
      >
        <div className="h-full w-64">
          <SidebarContent />
        </div>
      </aside>
      <MobileSidebar />
      <main className="relative flex min-h-0 min-w-0 flex-1 flex-col bg-background">
        {route.view === "thread" ? (
          <ThreadView threadId={route.threadId} />
        ) : route.view === "new" ? (
          <NewThread projectPath={route.projectPath} agentId={route.agentId} />
        ) : route.view === "settings" ? (
          <SettingsView section={route.section} />
        ) : (
          <EmptyState />
        )}
      </main>
      <CommandPalette />
      <WindowControls />
      <ConnectionBanner />
      <Toasts />
    </div>
  );
}
