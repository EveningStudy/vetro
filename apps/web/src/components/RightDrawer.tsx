import { ArrowLeftIcon, CircleAlertIcon, CircleCheckIcon, CornerDownRightIcon, ExternalLinkIcon, XIcon } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { ThreadSummary } from "~/api/types";
import { useT } from "~/i18n";
import { relativeTime } from "~/lib/format";
import { cn } from "~/lib/utils";
import { DRAWER_MIN, clampDrawerWidth, setDrawerWidth, usePrefs } from "~/state/prefs";
import { actions, useStore } from "~/state/store";
import { AgentIcon } from "./AgentIcon";
import { Timeline } from "./chat/Timeline";
import { ChangesPanel } from "./drawer/ChangesPanel";
import { PreviewPanel } from "./drawer/PreviewPanel";
import { useMediaQuery, useNow } from "./hooks";
import { RawLogPanel } from "./RawLogPanel";
import { Button } from "./ui/button";
import { Tip } from "./ui/overlay";
import { Spinner } from "./ui/primitives";
import { useHeaderChromeClass, useWindowDragProps } from "./WindowChrome";

const MAX_VIEWPORT_SHARE = 0.72;

function maxWidth() {
  return Math.max(DRAWER_MIN, Math.floor(window.innerWidth * MAX_VIEWPORT_SHARE));
}

/** The single right-side inspector slot. Each drawer kind renders its own header and body. */
export function RightDrawer({ threadId }: { threadId: string }) {
  const drawer = useStore((s) => s.rightDrawer);
  const saved = usePrefs((s) => s.drawerWidth);
  const desktop = useMediaQuery("(min-width: 48rem)");
  const [dragWidth, setDragWidth] = useState<number | null>(null);
  const drag = useRef<{ x: number; width: number } | null>(null);
  const t = useT();
  if (!drawer) return null;

  const width = Math.min(dragWidth ?? saved, maxWidth());
  const onPointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    drag.current = { x: event.clientX, width };
    setDragWidth(width);
  };
  const onPointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const start = drag.current;
    if (!start) return;
    setDragWidth(Math.min(maxWidth(), clampDrawerWidth(start.width + start.x - event.clientX)));
  };
  const onPointerUp = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    drag.current = null;
    event.currentTarget.releasePointerCapture(event.pointerId);
    if (dragWidth !== null) setDrawerWidth(dragWidth);
    setDragWidth(null);
  };

  return (
    <>
      <div className="fixed inset-0 z-40 bg-black/40 md:hidden" aria-hidden onClick={actions.closeDrawer} />
      <aside
        data-right-drawer={drawer.kind}
        style={desktop ? { width } : undefined}
        className={cn(
          "fixed inset-y-0 right-0 z-40 flex min-h-0 w-[min(100vw,32rem)] animate-drawer-in flex-col border-l bg-background pt-[env(safe-area-inset-top)] shadow-xl md:relative md:inset-auto md:z-auto md:shrink-0 md:pt-0 md:shadow-none",
          dragWidth === null && "md:transition-[width] md:duration-200 md:ease-(--ease-drawer)",
          dragWidth !== null && "select-none",
        )}
      >
        <div
          role="separator"
          aria-orientation="vertical"
          aria-label={t("drawer.resize")}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          onDoubleClick={() => setDrawerWidth(480)}
          className={cn(
            "absolute inset-y-0 -left-0.5 z-10 hidden w-1 cursor-col-resize transition-colors hover:bg-ring/40 md:block",
            dragWidth !== null && "bg-ring/50",
          )}
        />
        {drawer.kind === "raw" ? (
          <RawLogPanel threadId={threadId} />
        ) : drawer.kind === "changes" ? (
          <ChangesPanel threadId={threadId} />
        ) : drawer.kind === "children" ? (
          <ChildSessionsList key={drawer.parentId} parentId={drawer.parentId} />
        ) : drawer.kind === "preview" ? (
          <PreviewPanel key={`${drawer.cwd}\n${drawer.path}`} cwd={drawer.cwd} path={drawer.path} line={drawer.line} />
        ) : (
          <ChildSessionPeek key={drawer.threadId} threadId={drawer.threadId} fromParentId={drawer.fromParentId} />
        )}
      </aside>
    </>
  );
}

function ChildStatus({ thread }: { thread: ThreadSummary }) {
  if (thread.status === "running" || thread.status === "awaiting_input") return <Spinner className="size-3.5 shrink-0 text-info-foreground" />;
  if (thread.status === "error") return <CircleAlertIcon className="size-3.5 shrink-0 text-error" />;
  return <CircleCheckIcon className="size-3.5 shrink-0 text-success" />;
}

function ChildSessionsList({ parentId }: { parentId: string }) {
  const t = useT();
  const drag = useWindowDragProps();
  const chromeClass = useHeaderChromeClass("drawer");
  const now = useNow(30_000);
  const threads = useStore((s) => s.threads);
  const parent = useStore((s) => s.threads[parentId] ?? s.open[parentId]?.summary);
  const [loading, setLoading] = useState(true);
  const children = useMemo(
    () => Object.values(threads).filter((thread) => thread.parent?.threadId === parentId).sort((a, b) => (a.createdAt ?? a.updatedAt) - (b.createdAt ?? b.updatedAt)),
    [threads, parentId],
  );

  useEffect(() => {
    setLoading(true);
    actions
      .fetchChildren(parentId)
      .catch(() => undefined)
      .finally(() => setLoading(false));
  }, [parentId]);

  return (
    <>
      <div {...drag} className={cn("flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 border-b ps-4 pe-2", chromeClass)}>
        <CornerDownRightIcon className="size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1 truncate text-sm font-medium">
          {t("drawer.children", { n: Math.max(children.length, parent?.childCount ?? 0) })}
        </span>
        {loading ? <Spinner className="size-3.5 shrink-0 text-muted-foreground" /> : null}
        <Tip label={t("common.close")}>
          <Button size="icon-sm" variant="ghost" aria-label={t("common.close")} onClick={actions.closeDrawer}>
            <XIcon />
          </Button>
        </Tip>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto p-2">
        {children.length === 0 && !loading ? <p className="px-2 py-6 text-center text-sm text-muted-foreground">{t("drawer.noChildren")}</p> : null}
        {children.map((child) => (
          <button
            key={child.id}
            type="button"
            onClick={() => actions.openChildDrawer(child.id, parentId)}
            className="flex h-10 w-full min-w-0 cursor-pointer items-center gap-2.5 rounded-md px-2 text-left outline-none hover:bg-accent/60 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
          >
            <AgentIcon agentId={child.agentId} className="size-4 shrink-0" />
            <span className="min-w-0 flex-1 truncate text-sm">{child.title}</span>
            {child.parent?.callId === null ? (
              <span className="shrink-0 text-2xs text-muted-foreground/80">{t(`drawer.link.${child.parent.confidence}`)}</span>
            ) : null}
            <ChildStatus thread={child} />
            <span className="w-8 shrink-0 text-right text-xs tabular-nums text-muted-foreground">{relativeTime(child.updatedAt, t, now)}</span>
          </button>
        ))}
      </div>
    </>
  );
}

function ChildSessionPeek({ threadId, fromParentId }: { threadId: string; fromParentId?: string }) {
  const t = useT();
  const drag = useWindowDragProps();
  const chromeClass = useHeaderChromeClass("drawer");
  const summary = useStore((s) => s.threads[threadId] ?? s.open[threadId]?.summary);
  const agentId = summary?.agentId ?? threadId.split(":")[0]!;

  useEffect(() => {
    actions.retain(threadId);
    return () => actions.release(threadId);
  }, [threadId]);

  return (
    <>
      <div {...drag} className={cn("flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 border-b ps-2 pe-2", chromeClass)}>
        {fromParentId ? (
          <Tip label={t("drawer.backToList")}>
            <Button size="icon-sm" variant="ghost" aria-label={t("drawer.backToList")} onClick={() => actions.openChildrenDrawer(fromParentId)}>
              <ArrowLeftIcon />
            </Button>
          </Tip>
        ) : (
          <span className="w-2" />
        )}
        <AgentIcon agentId={agentId} className="size-4" />
        <span className="min-w-0 flex-1 truncate text-sm font-medium" title={summary?.title}>
          {summary?.title ?? t("drawer.child")}
        </span>
        {summary?.status === "running" ? <Spinner className="size-3.5 shrink-0 text-info-foreground" /> : null}
        <Button
          size="xs"
          variant="ghost-muted"
          onClick={() => {
            actions.closeDrawer();
            actions.navigate({ view: "thread", threadId });
          }}
        >
          <ExternalLinkIcon />
          <span className="max-sm:hidden">{t("drawer.openFull")}</span>
        </Button>
        <Tip label={t("common.close")}>
          <Button size="icon-sm" variant="ghost" aria-label={t("common.close")} onClick={actions.closeDrawer}>
            <XIcon />
          </Button>
        </Tip>
      </div>
      <Timeline threadId={threadId} />
    </>
  );
}
