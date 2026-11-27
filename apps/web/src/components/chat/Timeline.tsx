import { ArrowDownIcon, LayersIcon } from "lucide-react";
import { Fragment, type ReactNode, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Item, ItemOf, RewindFilesResult } from "~/api/types";
import { type MessageKey, type Translate, useT } from "~/i18n";
import { formatDuration } from "~/lib/format";
import { actions, isCatalogStub, useStore } from "~/state/store";
import { errorMessage, toast } from "~/state/toast";
import { useNow } from "../hooks";
import { Button } from "../ui/button";
import { Dialog, DialogDescription, DialogPopup, DialogTitle } from "../ui/overlay";
import { Spinner } from "../ui/primitives";
import { ThreadCwdContext } from "./threadContext";
import { AgentMessage, AnswerFooter, NoticeRow, PlanCard, Reasoning, RequestItem, ToolCallRow, TurnEnd, UserMessage } from "./Items";
import { diffStats } from "./ToolDetails";
import { WorkLogRow } from "./WorkLog";

const INITIAL_WINDOW = 120;
const PAGE = 80;
const BOTTOM_SLACK = 48;

/** Where the reader left each thread, so coming back does not jump to the bottom. */
const scrollMemory = new Map<string, { top: number; atBottom: boolean; start: number | null }>();
/** Preserves active-turn elapsed clock across WorkingRow hide/show within the same turn. */
const activeRunStarts = new Map<string, { turnKey: string; startedAt: number }>();

function resolveWorkingStart(threadId: string, turnKey: string, seedTs: number | null, remote: boolean) {
  const existing = activeRunStarts.get(threadId);
  if (existing && existing.turnKey === turnKey) return existing.startedAt;
  const now = Date.now();
  const startedAt = !remote && seedTs !== null && Math.abs(now - seedTs) < 3_600_000 ? Math.min(now, seedTs) : now;
  activeRunStarts.set(threadId, { turnKey, startedAt });
  return startedAt;
}

export function isPendingRequest(item: Item): item is ItemOf<"request"> {
  return item.type === "request" && item.resolution === null;
}

function isLogRow(item: Item) {
  return (
    item.type === "reasoning" ||
    item.type === "tool_call" ||
    item.type === "notice" ||
    (item.type === "request" && item.resolution !== null)
  );
}

function isStartupConfigNotice(text: string) {
  return (
    text.includes("unrecognized configuration setting") ||
    text.includes("Under-development features enabled:") ||
    text.includes("suppress_unstable_features_warning")
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
      return item.level === "info" || (item.level === "warning" && isStartupConfigNotice(item.text));
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

function isUnsettled(item: Item) {
  return (
    ((item.type === "agent_message" || item.type === "reasoning") && item.streaming) ||
    (item.type === "tool_call" && (item.status === "in_progress" || item.status === "pending")) ||
    isPendingRequest(item)
  );
}

/**
 * Answer text per turn. A real `turn_end` owns its answer; a turn that the native history
 * closed without one gets a copy-only footer on its last settled answer.
 */
function answersByTurn(items: Item[], running: boolean) {
  const ends = new Map<string, string>();
  const fallbacks = new Map<string, string>();
  let texts: string[] = [];
  let lastAnswer: string | null = null;
  let unsettled = false;
  const close = (final: boolean) => {
    if (lastAnswer && texts.length && !unsettled && !(final && running)) fallbacks.set(lastAnswer, texts.join("\n\n"));
    texts = [];
    lastAnswer = null;
    unsettled = false;
  };
  for (const item of items) {
    if (item.type === "user_message") {
      close(false);
    } else if (item.type === "turn_end") {
      ends.set(item.id, texts.join("\n\n"));
      texts = [];
      lastAnswer = null;
      unsettled = false;
    } else {
      if (isUnsettled(item)) unsettled = true;
      if (item.type === "agent_message" && !item.streaming && item.text.trim()) {
        texts.push(item.text);
        lastAnswer = item.id;
      }
    }
  }
  close(true);
  return { ends, fallbacks };
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
  if (item.type === "turn_end") return "mt-1.5";
  if (before.type === "turn_end") return "mt-6";
  if (item.type === "user_message") return "mt-6";
  if (isLogRow(before) && isLogRow(item)) return "";
  if (isLogRow(item) || isLogRow(before)) return "mt-2";
  return "mt-4";
}

function ItemView({
  item,
  threadId,
  answerText,
  isLastTurn,
  onFork,
  forkLabel,
  onRollback,
  onRewindFiles,
}: {
  item: Item;
  threadId: string;
  answerText?: string;
  isLastTurn?: boolean;
  onFork?: () => void;
  forkLabel?: string;
  onRollback?: () => void;
  onRewindFiles?: () => void;
}) {
  switch (item.type) {
    case "user_message":
      return <UserMessage item={item} onRollback={onRollback} onRewindFiles={onRewindFiles} />;
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
      return <TurnEnd item={item} answerText={answerText} isLast={isLastTurn} onFork={onFork} forkLabel={forkLabel} />;
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

function WorkingRow({ threadId, turnKey, seedTs }: { threadId: string; turnKey: string; seedTs: number | null }) {
  const t = useT();
  const remote = useStore((s) => !!s.app?.remoteClient);
  const now = useNow(1000);
  const [startedAt] = useState(() => resolveWorkingStart(threadId, turnKey, seedTs, remote));
  const elapsed = formatDuration(Math.max(0, now - startedAt));
  return (
    <div className="mt-2">
      <WorkLogRow
        icon={<Spinner className="size-3.5" />}
        label={<span className="live-tool-shine">{t("item.working", { duration: elapsed })}</span>}
      />
    </div>
  );
}

function TimelineRows({ items, threadId, dockRequests }: { items: Item[]; threadId: string; dockRequests: boolean }) {
  const t = useT();
  const cwd = useStore((s) => s.open[threadId]?.summary.cwd ?? null);
  const threadStatus = useStore((s) => s.open[threadId]?.summary.status);
  const running = threadStatus === "running" || threadStatus === "awaiting_input";
  const working = threadStatus === "running";
  const forkCaps = useStore((s) => {
    const summary = s.threads[threadId] ?? s.open[threadId]?.summary;
    if (!summary?.resumable || summary.status === "running" || summary.status === "awaiting_input") return 0;
    const fork = s.agents.find((agent) => agent.id === summary.agentId)?.capabilities.fork;
    return (fork?.atTurn ? 1 : 0) | (fork?.whole ? 2 : 0);
  });
  const canForkAtTurn = (forkCaps & 1) !== 0;
  const canForkWhole = (forkCaps & 2) !== 0;
  const historyCaps = useStore((s) => {
    const summary = s.threads[threadId] ?? s.open[threadId]?.summary;
    if (!summary?.resumable || summary.status === "running" || summary.status === "awaiting_input") return 0;
    const edit = s.agents.find((agent) => agent.id === summary.agentId)?.capabilities.historyEdit;
    return (edit?.rollbackConversation ? 1 : 0) | (edit?.rewindFiles ? 2 : 0) | (edit?.previewRewindFiles ? 4 : 0);
  });
  const canRollback = (historyCaps & 1) !== 0;
  const canRewind = (historyCaps & 2) !== 0;
  const canPreviewRewind = (historyCaps & 4) !== 0;
  const [historyEdit, setHistoryEdit] = useState<HistoryEditTarget | null>(null);
  const segments = useMemo(
    () =>
      segment(
        items.filter(
          (item) =>
            (item.type !== "agent_message" || item.streaming || item.text.trim() !== "") &&
            (item.type !== "reasoning" || item.streaming || item.text.trim() !== "") &&
            !(dockRequests && isPendingRequest(item)),
        ),
        running,
      ),
    [items, running, dockRequests],
  );
  const answers = useMemo(() => answersByTurn(items, running), [items, running]);

  const lastTurnEndId = useMemo(() => {
    for (let index = segments.length - 1; index >= 0; index -= 1) {
      const entry = segments[index]!;
      if (entry.kind === "item" && entry.item.type === "turn_end") return entry.item.id;
    }
    return null;
  }, [segments]);

  const forkFor = (item: Item, isLastTurn: boolean): { onFork?: () => void; forkLabel?: string } => {
    if (item.type !== "turn_end") return {};
    const turnId = item.turnId;
    if (canForkAtTurn && turnId) return { onFork: () => void actions.forkThread(threadId, turnId), forkLabel: t("item.forkFromHere") };
    if (canForkWhole && isLastTurn) return { onFork: () => void actions.forkThread(threadId), forkLabel: t("thread.fork") };
    return {};
  };

  const { turnKey, seedTs, hasVisibleLive } = useMemo(() => {
    let lastEndIdx = -1;
    for (let index = items.length - 1; index >= 0; index -= 1) {
      if (items[index]!.type === "turn_end") {
        lastEndIdx = index;
        break;
      }
    }
    const tail = items.slice(lastEndIdx + 1);
    const firstUser = tail.find((entry) => entry.type === "user_message");
    return {
      turnKey: lastEndIdx >= 0 ? items[lastEndIdx]!.id : "init",
      seedTs: firstUser?.ts ?? null,
      hasVisibleLive: tail.some(isUnsettled),
    };
  }, [items]);

  const showWorkingRow = working && !hasVisibleLive;

  return (
    <ThreadCwdContext value={cwd}>
      {segments.map((current, index) => {
        if (current.kind !== "item") {
          return (
            <div key={`group:${current.id}`} className={spacing(segments[index - 1], current)}>
              <WorkGroup items={current.items} live={current.live} threadId={threadId} />
            </div>
          );
        }
        const isLastTurn = current.item.type === "turn_end" && current.item.id === lastTurnEndId;
        const fork = forkFor(current.item, isLastTurn);
        const item = current.item;
        const turnId = item.turnId;
        const isUser = item.type === "user_message";
        return (
          <div key={item.id} data-item-id={item.id} className={spacing(segments[index - 1], current)}>
            <ItemView
              item={item}
              threadId={threadId}
              answerText={item.type === "turn_end" ? answers.ends.get(item.id) : undefined}
              isLastTurn={isLastTurn}
              onFork={fork.onFork}
              forkLabel={fork.forkLabel}
              onRollback={isUser && canRollback && turnId ? () => setHistoryEdit({ kind: "rollback", turnId }) : undefined}
              onRewindFiles={isUser && canRewind ? () => setHistoryEdit({ kind: "rewind", userMessageId: item.id }) : undefined}
            />
            {item.type === "agent_message" && answers.fallbacks.has(item.id) ? (
              <div className="mt-1.5">
                <AnswerFooter answerText={answers.fallbacks.get(item.id)!} />
              </div>
            ) : null}
          </div>
        );
      })}
      {showWorkingRow ? <WorkingRow key={`${threadId}:${turnKey}`} threadId={threadId} turnKey={turnKey} seedTs={seedTs} /> : null}
      {historyEdit?.kind === "rollback" ? (
        <RollbackDialog threadId={threadId} turnId={historyEdit.turnId} onClose={() => setHistoryEdit(null)} />
      ) : historyEdit?.kind === "rewind" ? (
        <RewindFilesDialog
          threadId={threadId}
          userMessageId={historyEdit.userMessageId}
          canPreview={canPreviewRewind}
          onClose={() => setHistoryEdit(null)}
        />
      ) : null}
    </ThreadCwdContext>
  );
}

type HistoryEditTarget = { kind: "rollback"; turnId: string } | { kind: "rewind"; userMessageId: string };

function RollbackDialog({ threadId, turnId, onClose }: { threadId: string; turnId: string; onClose: () => void }) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const confirm = () => {
    setBusy(true);
    void actions.rollbackThread(threadId, turnId).then((ok) => {
      setBusy(false);
      if (ok) onClose();
    });
  };
  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogPopup className="max-w-md" showClose={!busy}>
        <div className="flex flex-col gap-4 p-5">
          <div className="flex flex-col gap-1.5 pe-8">
            <DialogTitle>{t("history.rollbackTitle")}</DialogTitle>
            <DialogDescription>{t("history.rollbackBody")}</DialogDescription>
          </div>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" disabled={busy} onClick={onClose}>
              {t("common.cancel")}
            </Button>
            <Button variant="destructive" disabled={busy} onClick={confirm}>
              {busy ? <Spinner className="size-3.5" /> : null}
              {t("history.rollbackConfirm")}
            </Button>
          </div>
        </div>
      </DialogPopup>
    </Dialog>
  );
}

type RewindState = { phase: "loading" } | { phase: "ready"; result: RewindFilesResult | null } | { phase: "error"; message: string };

function RewindFilesDialog({
  threadId,
  userMessageId,
  canPreview,
  onClose,
}: {
  threadId: string;
  userMessageId: string;
  canPreview: boolean;
  onClose: () => void;
}) {
  const t = useT();
  const [state, setState] = useState<RewindState>(canPreview ? { phase: "loading" } : { phase: "ready", result: null });
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!canPreview) return;
    let cancelled = false;
    actions
      .rewindFiles(threadId, userMessageId, true)
      .then((result) => !cancelled && setState({ phase: "ready", result }))
      .catch((error: unknown) => !cancelled && setState({ phase: "error", message: errorMessage(error) }));
    return () => {
      cancelled = true;
    };
  }, [threadId, userMessageId, canPreview]);

  const confirm = () => {
    setBusy(true);
    actions
      .rewindFiles(threadId, userMessageId, false)
      .then((result) => {
        if (result.canRewind) {
          toast.success(t("history.rewound"));
          onClose();
        } else setState({ phase: "ready", result });
      })
      .catch((error: unknown) => setState({ phase: "error", message: errorMessage(error) }))
      .finally(() => setBusy(false));
  };

  const preview = state.phase === "ready" ? state.result : null;
  const failure =
    state.phase === "error" ? state.message : preview && !preview.canRewind ? (preview.error ?? t("history.rewindUnavailable")) : null;
  const ready = state.phase === "ready" && !failure;

  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogPopup className="max-w-md" showClose={!busy}>
        <div className="flex min-h-0 flex-col gap-4 p-5">
          <div className="flex flex-col gap-1.5 pe-8">
            <DialogTitle>{t("history.rewindTitle")}</DialogTitle>
            {ready ? <DialogDescription>{t("history.rewindBody")}</DialogDescription> : null}
          </div>
          {state.phase === "loading" ? (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Spinner className="size-3.5" />
              {t("history.checking")}
            </div>
          ) : failure ? (
            <p className="rounded-lg border border-destructive/24 bg-destructive/4 px-3 py-2 text-sm text-destructive-foreground">{failure}</p>
          ) : preview ? (
            <RewindPreview result={preview} />
          ) : null}
          <div className="flex justify-end gap-2">
            {ready ? (
              <>
                <Button variant="ghost" disabled={busy} onClick={onClose}>
                  {t("common.cancel")}
                </Button>
                <Button disabled={busy} onClick={confirm}>
                  {busy ? <Spinner className="size-3.5" /> : null}
                  {t("history.rewindConfirm")}
                </Button>
              </>
            ) : (
              <Button variant="ghost" onClick={onClose}>
                {t("common.close")}
              </Button>
            )}
          </div>
        </div>
      </DialogPopup>
    </Dialog>
  );
}

function RewindPreview({ result }: { result: RewindFilesResult }) {
  const t = useT();
  const files = result.filesChanged;
  return (
    <div className="flex min-h-0 flex-col gap-2">
      {files.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("history.rewindNoFiles")}</p>
      ) : (
        <>
          <div className="flex items-baseline justify-between gap-2 text-xs text-muted-foreground">
            <span>{t("history.rewindFileList", { n: files.length })}</span>
            {result.insertions || result.deletions ? (
              <span className="font-mono tabular-nums">
                <span className="text-diff-addition-foreground">+{result.insertions ?? 0}</span>{" "}
                <span className="text-diff-deletion-foreground">−{result.deletions ?? 0}</span>
              </span>
            ) : null}
          </div>
          <ul className="max-h-48 overflow-y-auto rounded-lg border bg-muted/40 px-3 py-2 font-mono text-xs">
            {files.map((file) => (
              <li key={file} className="truncate py-0.5" title={file}>
                {file}
              </li>
            ))}
          </ul>
        </>
      )}
      {result.skippedLinks.length > 0 ? (
        <div className="rounded-lg border border-warning/32 bg-warning-surface px-3 py-2 text-xs text-warning-foreground">
          <p>{t("history.rewindSkipped")}</p>
          <ul className="mt-1 font-mono">
            {result.skippedLinks.map((link) => (
              <li key={link} className="truncate" title={link}>
                {link}
              </li>
            ))}
          </ul>
        </div>
      ) : null}
    </div>
  );
}

/** A thread's items. The timeline owns the scroller: it windows long threads, loads older
 * pages on demand and stays pinned to the bottom while streaming unless the user scrolled up. */
export function Timeline({ threadId, footer, dockRequests = false }: { threadId: string; footer?: React.ReactNode; dockRequests?: boolean }) {
  return <ScrollingTimeline key={threadId} threadId={threadId} footer={footer} dockRequests={dockRequests} />;
}

function ScrollingTimeline({ threadId, footer, dockRequests }: { threadId: string; footer?: React.ReactNode; dockRequests: boolean }) {
  const t = useT();
  const thread = useStore((s) => s.open[threadId]);
  const items = useMemo(() => thread?.items ?? [], [thread?.items]);
  const scrollerRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const sentinelRef = useRef<HTMLDivElement>(null);
  const [memory] = useState(() => scrollMemory.get(threadId));
  const atBottomRef = useRef(memory?.atBottom ?? true);
  const restoredRef = useRef(false);
  const anchorRef = useRef<{ height: number; top: number } | null>(null);
  const [atBottom, setAtBottom] = useState(memory?.atBottom ?? true);
  const [startOrdinal, setStartOrdinal] = useState<number | null>(memory?.start ?? null);

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

  const loaded = !!thread;
  const threadStatus = thread?.summary.status;
  useEffect(() => {
    if (threadStatus && threadStatus !== "running" && threadStatus !== "awaiting_input") {
      activeRunStarts.delete(threadId);
    }
  }, [threadId, threadStatus]);
  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller || restoredRef.current) return;
    if (memory && !memory.atBottom) {
      if (!loaded) return;
      scroller.scrollTop = memory.top;
    } else scroller.scrollTop = scroller.scrollHeight;
    if (loaded) restoredRef.current = true;
  }, [loaded, memory]);

  useEffect(() => {
    scrollMemory.set(threadId, { ...(scrollMemory.get(threadId) ?? { top: 0, atBottom: true }), start: startOrdinal });
  }, [threadId, startOrdinal]);

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

  const scrollSignal = thread?.scrollSignal ?? 0;
  const latestUserMessageId = useMemo(() => {
    for (let index = items.length - 1; index >= 0; index -= 1) {
      if (items[index]!.type === "user_message") return items[index]!.id;
    }
    return null;
  }, [items]);
  const prevScrollSignalRef = useRef(scrollSignal);
  const prevUserMsgIdRef = useRef(latestUserMessageId);
  useLayoutEffect(() => {
    const signalChanged = scrollSignal !== prevScrollSignalRef.current;
    const userMsgChanged = latestUserMessageId !== null && latestUserMessageId !== prevUserMsgIdRef.current;
    prevScrollSignalRef.current = scrollSignal;
    prevUserMsgIdRef.current = latestUserMessageId;
    if (!signalChanged && !userMsgChanged) return;
    atBottomRef.current = true;
    setAtBottom(true);
    const scroller = scrollerRef.current;
    if (scroller) {
      scroller.scrollTop = scroller.scrollHeight;
      requestAnimationFrame(() => {
        if (atBottomRef.current && scrollerRef.current) {
          scrollerRef.current.scrollTop = scrollerRef.current.scrollHeight;
        }
      });
    }
  }, [scrollSignal, latestUserMessageId]);

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
          if (restoredRef.current) scrollMemory.set(threadId, { top: el.scrollTop, atBottom: bottom, start: startOrdinal });
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
            thread.summary.status === "running" ? (
              <WorkingRow key={`${threadId}:empty`} threadId={threadId} turnKey="empty" seedTs={null} />
            ) : (
              <div className="mx-auto max-w-md py-16 text-center text-sm text-muted-foreground">
                {isCatalogStub(thread.summary) ? (
                  <>
                    <p>{thread.summary.origin === "Codex desktop catalog (remote)" ? t("thread.emptyCatalogRemote") : t("thread.emptyCatalogCloud")}</p>
                    {thread.summary.preview ? (
                      <p className="mt-2 break-all font-mono text-xs text-muted-foreground/80">{thread.summary.preview}</p>
                    ) : null}
                  </>
                ) : (
                  t("thread.empty")
                )}
              </div>
            )
          ) : (
            <Fragment>
              <TimelineRows items={visible} threadId={threadId} dockRequests={dockRequests} />
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
