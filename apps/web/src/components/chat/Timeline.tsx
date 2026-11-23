import { ArrowDownIcon } from "lucide-react";
import { Fragment, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Item } from "~/api/types";
import { useT } from "~/i18n";
import { actions, useStore } from "~/state/store";
import { Button } from "../ui/button";
import { Spinner } from "../ui/primitives";
import { ThreadCwdContext } from "./threadContext";
import { AgentMessage, NoticeRow, PlanCard, Reasoning, RequestItem, ToolCallRow, TurnEnd, UserMessage } from "./Items";

const INITIAL_WINDOW = 120;
const PAGE = 80;
const BOTTOM_SLACK = 48;

const expandChildrenByDefault = new URLSearchParams(location.search).get("expand") === "children";

function isLogRow(item: Item) {
  return (
    item.type === "reasoning" ||
    item.type === "tool_call" ||
    item.type === "notice" ||
    (item.type === "request" && item.resolution !== null)
  );
}

function spacing(previous: Item | undefined, item: Item) {
  if (!previous) return "";
  if (item.type === "turn_end") return "mt-3";
  if (previous.type === "turn_end") return "mt-5";
  if (item.type === "user_message") return "mt-6";
  if (isLogRow(previous) && isLogRow(item)) return "";
  if (isLogRow(item) || isLogRow(previous)) return "mt-2";
  return "mt-4";
}

function ItemView({ item, threadId, expandChildren }: { item: Item; threadId: string; expandChildren: boolean }) {
  switch (item.type) {
    case "user_message":
      return <UserMessage item={item} />;
    case "agent_message":
      return <AgentMessage item={item} />;
    case "reasoning":
      return <Reasoning item={item} />;
    case "tool_call":
      return <ToolCallRow item={item} expandChildren={expandChildren} />;
    case "plan":
      return <PlanCard item={item} />;
    case "request":
      return <RequestItem item={item} threadId={threadId} />;
    case "notice":
      return <NoticeRow item={item} />;
    case "turn_end":
      return <TurnEnd item={item} />;
  }
}

function TimelineRows({
  items,
  threadId,
  expandChildren,
}: {
  items: Item[];
  threadId: string;
  expandChildren: boolean;
}) {
  const cwd = useStore((s) => s.open[threadId]?.summary.cwd ?? null);
  const rows = items.filter((item) => item.type !== "agent_message" || item.streaming || item.text.trim() !== "");
  return (
    <ThreadCwdContext value={cwd}>
      {rows.map((item, index) => (
        <div
          key={item.id}
          data-item-id={item.id}
          className={spacing(rows[index - 1], item)}
        >
          <ItemView item={item} threadId={threadId} expandChildren={expandChildren} />
        </div>
      ))}
    </ThreadCwdContext>
  );
}

/** A thread's items. Top-level timelines own the scroller: they window long threads, load older
 * pages on demand and stay pinned to the bottom while streaming unless the user scrolled up. */
export function Timeline({ threadId, nested = false, footer }: { threadId: string; nested?: boolean; footer?: React.ReactNode }) {
  const t = useT();
  const thread = useStore((s) => s.open[threadId]);
  const items = thread?.items;

  if (nested) {
    if (!items) return null;
    return items.length === 0 ? (
      <div className="py-2 text-sm text-muted-foreground">{t("thread.empty")}</div>
    ) : (
      <TimelineRows items={items} threadId={threadId} expandChildren={false} />
    );
  }
  return <ScrollingTimeline key={threadId} threadId={threadId} footer={footer} />;
}

function ScrollingTimeline({ threadId, footer }: { threadId: string; footer?: React.ReactNode }) {
  const t = useT();
  const thread = useStore((s) => s.open[threadId]);
  const items = useMemo(() => thread?.items ?? [], [thread?.items]);
  const scrollerRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const sentinelRef = useRef<HTMLDivElement>(null);
  const atBottomRef = useRef(true);
  const anchorRef = useRef<{ height: number; top: number } | null>(null);
  const [atBottom, setAtBottom] = useState(true);
  const [startOrdinal, setStartOrdinal] = useState<number | null>(null);

  const effectiveStart = startOrdinal ?? items[Math.max(0, items.length - INITIAL_WINDOW)]?.ordinal ?? 0;
  const firstVisible = items.findIndex((item) => item.ordinal >= effectiveStart);
  const visible = firstVisible < 0 ? [] : items.slice(firstVisible);
  const hiddenLocally = Math.max(0, firstVisible);
  const canLoadMore = hiddenLocally > 0 || !!thread?.hasMore;

  const rememberAnchor = () => {
    const scroller = scrollerRef.current;
    if (scroller) anchorRef.current = { height: scroller.scrollHeight, top: scroller.scrollTop };
  };

  const revealOlder = useCallback(async () => {
    const current = useStore.getState().open[threadId];
    if (!current) return;
    const all = current.items;
    const first = all.findIndex((item) => item.ordinal >= effectiveStart);
    if (first > 0) {
      rememberAnchor();
      setStartOrdinal(all[Math.max(0, first - PAGE)]!.ordinal);
      return;
    }
    if (!current.hasMore || current.loadingOlder) return;
    await actions.loadOlder(threadId);
    const after = useStore.getState().open[threadId]?.items ?? [];
    const index = after.findIndex((item) => item.ordinal >= effectiveStart);
    rememberAnchor();
    setStartOrdinal(after[Math.max(0, index - PAGE)]?.ordinal ?? effectiveStart);
  }, [threadId, effectiveStart]);

  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    const anchor = anchorRef.current;
    if (!scroller || !anchor) return;
    anchorRef.current = null;
    scroller.scrollTop = anchor.top + (scroller.scrollHeight - anchor.height);
  }, [effectiveStart, items.length]);

  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
  }, []);

  useEffect(() => {
    const content = contentRef.current;
    const scroller = scrollerRef.current;
    if (!content || !scroller) return;
    const observer = new ResizeObserver(() => {
      if (atBottomRef.current && !anchorRef.current) scroller.scrollTop = scroller.scrollHeight;
    });
    observer.observe(content);
    return () => observer.disconnect();
  }, []);

  const lastItem = items.at(-1);
  useEffect(() => {
    if (lastItem?.type !== "user_message") return;
    atBottomRef.current = true;
    setAtBottom(true);
    const scroller = scrollerRef.current;
    if (scroller) scroller.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" });
  }, [lastItem?.id, lastItem?.type]);

  useEffect(() => {
    const sentinel = sentinelRef.current;
    const scroller = scrollerRef.current;
    if (!sentinel || !scroller || !canLoadMore) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting) && scroller.scrollTop > 0) void revealOlder();
      },
      { root: scroller, rootMargin: "600px 0px 0px 0px" },
    );
    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [canLoadMore, revealOlder]);

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      <div
        ref={scrollerRef}
        onScroll={(event) => {
          const el = event.currentTarget;
          const bottom = el.scrollHeight - el.scrollTop - el.clientHeight < BOTTOM_SLACK;
          atBottomRef.current = bottom;
          if (bottom !== atBottom) setAtBottom(bottom);
        }}
        className="min-h-0 flex-1 overflow-y-auto overscroll-contain [overflow-anchor:none] [scrollbar-gutter:stable]"
      >
        <div ref={contentRef} className="mx-auto w-full max-w-(--chat-content-max-width) px-(--workspace-gutter) pt-4 pb-8">
          <div ref={sentinelRef} />
          {canLoadMore ? (
            <div className="mb-4 flex justify-center">
              <Button size="xs" variant="ghost-muted" onClick={() => void revealOlder()} disabled={thread?.loadingOlder}>
                {thread?.loadingOlder ? <Spinner className="size-3" /> : null}
                {thread?.loadingOlder ? t("thread.loadingOlder") : t("thread.loadOlder")}
              </Button>
            </div>
          ) : null}
          {!thread ? (
            <div className="flex justify-center py-16">
              <Spinner className="text-muted-foreground" />
            </div>
          ) : visible.length === 0 ? (
            <div className="py-16 text-center text-sm text-muted-foreground">{t("thread.empty")}</div>
          ) : (
            <Fragment>
              <TimelineRows items={visible} threadId={threadId} expandChildren={expandChildrenByDefault} />
            </Fragment>
          )}
          {footer}
        </div>
      </div>
      {!atBottom ? (
        <div className="pointer-events-none absolute inset-x-0 bottom-3 flex justify-center">
          <Button
            size="xs"
            variant="outline"
            className="pointer-events-auto rounded-full shadow-md"
            onClick={() => {
              const scroller = scrollerRef.current;
              if (scroller) scroller.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" });
            }}
          >
            <ArrowDownIcon />
            {t("thread.jumpLatest")}
          </Button>
        </div>
      ) : null}
    </div>
  );
}
