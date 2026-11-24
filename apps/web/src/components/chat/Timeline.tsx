import { ArrowDownIcon, LayersIcon } from "lucide-react";
import { Fragment, type ReactNode, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Item, ItemOf } from "~/api/types";
import { type MessageKey, type Translate, useT } from "~/i18n";
import { actions, useStore } from "~/state/store";
import { Button } from "../ui/button";
import { Spinner } from "../ui/primitives";
import { ThreadCwdContext } from "./threadContext";
import { AgentMessage, NoticeRow, PlanCard, Reasoning, RequestItem, ToolCallRow, TurnEnd, UserMessage } from "./Items";
import { diffStats } from "./ToolDetails";
import { WorkLogRow } from "./WorkLog";

const INITIAL_WINDOW = 120;
const PAGE = 80;
const BOTTOM_SLACK = 48;

function isLogRow(item: Item) {
  return (
    item.type === "reasoning" ||
    item.type === "tool_call" ||
    item.type === "notice" ||
    (item.type === "request" && item.resolution !== null)
  );
}

/** Intermediate work that folds into a group; everything else stays on the timeline. */
function isFoldable(item: Item) {
  switch (item.type) {
    case "reasoning":
      return true;
    case "tool_call":
      return item.status !== "failed" && item.childThreadIds.length === 0;
    case "notice":
      return item.level === "info";
    case "request":
      return item.resolution !== null;
    default:
      return false;
  }
}

type Segment = { kind: "item"; item: Item } | { kind: "group"; id: string; items: Item[]; live: boolean };

function isLive(item: Item) {
  if (item.type === "reasoning") return item.streaming;
  if (item.type === "tool_call") return item.status === "in_progress" || item.status === "pending";
  return false;
}

function segment(items: Item[], running: boolean): Segment[] {
  const segments: Segment[] = [];
  for (const item of items) {
    const last = segments.at(-1);
    if (!isFoldable(item)) segments.push({ kind: "item", item });
    else if (last?.kind === "group") last.items.push(item);
    else segments.push({ kind: "group", id: item.id, items: [item], live: false });
  }
  if (!running) return segments;
  for (let index = segments.length - 1; index >= 0; index -= 1) {
    const current = segments[index]!;
    if (current.kind === "item") {
      const item = current.item;
      if (item.type === "turn_end" || (item.type === "agent_message" && !item.streaming)) break;
      continue;
    }
    current.live = isLive(current.items.at(-1)!);
    break;
  }
  return segments;
}

function first(segment: Segment) {
  return segment.kind === "item" ? segment.item : segment.items[0]!;
}

function last(segment: Segment) {
  return segment.kind === "item" ? segment.item : segment.items.at(-1)!;
}

function spacing(previous: Segment | undefined, current: Segment) {
  if (!previous) return "";
  const before = last(previous);
  const item = first(current);
  if (item.type === "turn_end") return "mt-3";
  if (before.type === "turn_end") return "mt-5";
  if (item.type === "user_message") return "mt-6";
  if (isLogRow(before) && isLogRow(item)) return "";
  if (isLogRow(item) || isLogRow(before)) return "mt-2";
  return "mt-4";
}

function ItemView({ item, threadId }: { item: Item; threadId: string }) {
  switch (item.type) {
    case "user_message":
      return <UserMessage item={item} />;
    case "agent_message":
      return <AgentMessage item={item} />;
    case "reasoning":
      return <Reasoning item={item} />;
    case "tool_call":
      return <ToolCallRow item={item} />;
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

function plural(t: Translate, n: number, one: MessageKey, many: MessageKey) {
  return n === 1 ? t(one) : t(many, { n });
}

function workSummary(items: Item[], t: Translate): ReactNode[] {
  const tools = items.filter((item): item is ItemOf<"tool_call"> => item.type === "tool_call");
  const count = (kinds: string[]) => tools.filter((tool) => kinds.includes(tool.kind)).length;
  const read = count(["read", "search"]);
  const edits = tools.filter((tool) => ["edit", "delete", "move"].includes(tool.kind));
  const commands = count(["execute"]);
  const web = count(["fetch", "mcp"]);
  let additions = 0;
  let deletions = 0;
  for (const tool of edits) {
    const stats = diffStats(tool.content);
    additions += stats.additions;
    deletions += stats.deletions;
  }
  const parts: ReactNode[] = [plural(t, items.length, "group.workedOne", "group.worked")];
  if (read) parts.push(plural(t, read, "group.readOne", "group.read"));
  if (edits.length)
    parts.push(
      <span key="edits">
        {plural(t, edits.length, "group.editedOne", "group.edited")}
        {additions + deletions > 0 ? (
          <span className="ms-1 font-mono text-xs tabular-nums">
            (<span className="text-diff-addition-foreground">+{additions}</span>{" "}
            <span className="text-diff-deletion-foreground">−{deletions}</span>)
          </span>
        ) : null}
      </span>,
    );
  if (commands) parts.push(plural(t, commands, "group.ranOne", "group.ran"));
  if (web) parts.push(plural(t, web, "group.webOne", "group.web"));
  return parts;
}

function GroupItems({ items, threadId }: { items: Item[]; threadId: string }) {
  return (
    <div className="ms-3 mt-0.5 mb-1 border-l border-border/70 ps-2">
      {items.map((item) => (
        <div key={item.id} data-item-id={item.id}>
          <ItemView item={item} threadId={threadId} />
        </div>
      ))}
    </div>
  );
}

function WorkGroup({ items, live, threadId }: { items: Item[]; live: boolean; threadId: string }) {
  const t = useT();
  const [open, setOpen] = useState(false);
  if (items.length === 1) {
    return (
      <div data-item-id={items[0]!.id}>
        <ItemView item={items[0]!} threadId={threadId} />
      </div>
    );
  }

  if (live) {
    const done = items.slice(0, -1);
    const current = items.at(-1)!;
    return (
      <div data-work-group="live">
        <WorkLogRow
          icon={<LayersIcon className="size-4" />}
          expanded={open}
          onToggle={() => setOpen(!open)}
          label={<span className="text-secondary-label">{plural(t, done.length, "group.completedOne", "group.completed")}</span>}
        >
          {open ? <GroupItems items={done} threadId={threadId} /> : null}
        </WorkLogRow>
        <div data-item-id={current.id}>
          <ItemView item={current} threadId={threadId} />
        </div>
      </div>
    );
  }

  const parts = workSummary(items, t);
  return (
    <div data-work-group="done">
      <WorkLogRow
        icon={<LayersIcon className="size-4" />}
        expanded={open}
        onToggle={() => setOpen(!open)}
        label={
          <span className="text-secondary-label">
            {parts.map((part, index) => (
              <Fragment key={index}>
                {index > 0 ? <span className="text-muted-foreground/60"> · </span> : null}
                {part}
              </Fragment>
            ))}
          </span>
        }
      >
        {open ? <GroupItems items={items} threadId={threadId} /> : null}
      </WorkLogRow>
    </div>
  );
}

function TimelineRows({ items, threadId }: { items: Item[]; threadId: string }) {
  const cwd = useStore((s) => s.open[threadId]?.summary.cwd ?? null);
  const running = useStore((s) => {
    const status = s.open[threadId]?.summary.status;
    return status === "running" || status === "awaiting_input";
  });
  const segments = useMemo(
    () => segment(items.filter((item) => item.type !== "agent_message" || item.streaming || item.text.trim() !== ""), running),
    [items, running],
  );
  return (
    <ThreadCwdContext value={cwd}>
      {segments.map((current, index) =>
        current.kind === "item" ? (
          <div key={current.item.id} data-item-id={current.item.id} className={spacing(segments[index - 1], current)}>
            <ItemView item={current.item} threadId={threadId} />
          </div>
        ) : (
          <div key={`group:${current.id}`} className={spacing(segments[index - 1], current)}>
            <WorkGroup items={current.items} live={current.live} threadId={threadId} />
          </div>
        ),
      )}
    </ThreadCwdContext>
  );
}

/** A thread's items. The timeline owns the scroller: it windows long threads, loads older
 * pages on demand and stays pinned to the bottom while streaming unless the user scrolled up. */
export function Timeline({ threadId, footer }: { threadId: string; footer?: React.ReactNode }) {
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
              <TimelineRows items={visible} threadId={threadId} />
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
