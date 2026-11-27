import {
  ArchiveIcon,
  ArchiveRestoreIcon,
  ChevronRightIcon,
  CodeIcon,
  CopyIcon,
  CornerDownRightIcon,
  CornerLeftUpIcon,
  FileDiffIcon,
  FolderGit2Icon,
  FolderOpenIcon,
  GitBranchIcon,
  GitForkIcon,
  LockIcon,
  MoreHorizontalIcon,
  PencilIcon,
  PinIcon,
  PinOffIcon,
  ScrollTextIcon,
  SquareArrowOutUpRightIcon,
  SquareTerminalIcon,
  UnplugIcon,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { ThreadSummary } from "~/api/types";
import { useT } from "~/i18n";
import { basename, tildePath } from "~/lib/format";
import { cn, copyText, isMac } from "~/lib/utils";
import { type OpenThread, actions, useIsLocalDesktop, useStore } from "~/state/store";
import { rememberAgentOption, usePrefs } from "~/state/prefs";
import { toast } from "~/state/toast";
import { AgentIcon } from "../AgentIcon";
import { useEditedFiles } from "../drawer/ChangesPanel";
import { RightDrawer } from "../RightDrawer";
import { MobileSidebarButton } from "../Shell";
import { isQuickChatPath, isScratchOrTempPath } from "../sidebar/Sidebar";
import { Button } from "../ui/button";
import { Menu, MenuItem, MenuPopup, MenuSeparator, MenuTrigger, Tip } from "../ui/overlay";
import { Badge, Spinner } from "../ui/primitives";
import { useHeaderChromeClass, useWindowDragProps } from "../WindowChrome";
import { RpcFailure } from "~/api/protocol";
import { Composer } from "./Composer";
import { RequestItem } from "./Items";
import { SignInCard } from "./NewThread";
import { prepare } from "./OptionPicker";
import { sourceLabel } from "./sourceLabel";
import { Timeline, isPendingRequest } from "./Timeline";

let expandChildrenOnLoad = new URLSearchParams(location.search).get("expand") === "children";

export function ThreadView({ threadId }: { threadId: string }) {
  const t = useT();
  const thread = useStore((s) => s.open[threadId]);
  const summary = useStore((s) => s.threads[threadId] ?? s.open[threadId]?.summary);
  const [failure, setFailure] = useState<unknown>(null);
  const [attempt, setAttempt] = useState(0);
  const agent = useStore((s) => s.agents.find((a) => a.id === (summary?.agentId ?? threadId.split(":")[0])));

  useEffect(() => {
    setFailure(null);
    actions.retain(threadId);
    actions.openThread(threadId).catch((error: unknown) => setFailure(error ?? true));
    return () => actions.release(threadId);
  }, [threadId, attempt]);

  const firstChild = useStore((s) => {
    if (!expandChildrenOnLoad) return undefined;
    for (const item of s.open[threadId]?.items ?? []) if (item.type === "tool_call" && item.childThreadIds[0]) return item.childThreadIds[0];
    return undefined;
  });
  useEffect(() => {
    if (!firstChild || !expandChildrenOnLoad) return;
    expandChildrenOnLoad = false;
    actions.openChildDrawer(firstChild);
  }, [firstChild]);

  return (
    <div className="flex min-h-0 min-w-0 flex-1">
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        {summary ? <ThreadHeader thread={thread} threadId={threadId} /> : <BareHeader />}
        {failure && !thread ? (
          <div className="flex flex-1 flex-col items-center justify-center gap-3 px-(--workspace-gutter) text-sm text-muted-foreground">
            {needsAuth(failure) && agent ? (
              <SignInCard agent={agent} onDone={() => setAttempt((n) => n + 1)} />
            ) : (
              <>
                <span>{t("thread.notFound")}</span>
                {failure instanceof RpcFailure ? <span className="max-w-lg text-center text-xs">{failure.message}</span> : null}
                <Button size="sm" variant="outline" onClick={() => setAttempt((n) => n + 1)}>
                  {t("common.retry")}
                </Button>
              </>
            )}
          </div>
        ) : (
          <>
            <Timeline threadId={threadId} dockRequests />
            <div className="shrink-0 px-(--workspace-gutter) pb-[calc(0.75rem+env(safe-area-inset-bottom))]">
              {thread ? (
                <>
                  <DockedRequests thread={thread} />
                  <ThreadComposer thread={thread} />
                </>
              ) : null}
            </div>
          </>
        )}
      </div>
      <RightDrawer threadId={threadId} />
    </div>
  );
}

const rawLogShortcut = isMac ? "⌘⇧L" : "Ctrl+Shift+L";
const changesShortcut = isMac ? "⌘⇧D" : "Ctrl Shift D";

function editable(target: EventTarget | null) {
  if (!(target instanceof HTMLElement)) return null;
  if (target.isContentEditable) return target;
  return target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement ? target : null;
}

/** Questions and approvals the agent is waiting on sit above the composer, outside the scroller,
 * so reading back through the thread never hides them. */
function DockedRequests({ thread }: { thread: OpenThread }) {
  const pending = useMemo(() => thread.items.filter(isPendingRequest), [thread.items]);
  const threadId = thread.summary.id;
  const first = pending[0];

  useEffect(() => {
    if (!first || first.body.type !== "permission") return;
    const options = first.body.options;
    const onKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey || !/^[1-9]$/.test(event.key)) return;
      const field = editable(event.target);
      if (field && !(field instanceof HTMLTextAreaElement && field.value === "")) return;
      if (document.querySelector('[role="dialog"], [role="alertdialog"], [role="menu"], [role="listbox"]')) return;
      const option = options[Number(event.key) - 1];
      if (!option) return;
      event.preventDefault();
      void actions.respond(threadId, first.requestId, { type: "selected", optionId: option.id });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [first, threadId]);

  if (pending.length === 0) return null;
  return (
    <div className="mx-auto mb-2 flex max-h-[45vh] w-full max-w-(--chat-content-max-width) flex-col gap-2 overflow-y-auto">
      {pending.map((item, index) => (
        <div key={item.id} data-item-id={item.id} data-docked-request>
          <RequestItem item={item} threadId={threadId} shortcuts={index === 0} />
        </div>
      ))}
    </div>
  );
}

function BareHeader() {
  const drag = useWindowDragProps();
  const chromeClass = useHeaderChromeClass("main");
  return (
    <header {...drag} className={cn("flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 px-(--workspace-gutter)", chromeClass)}>
      <MobileSidebarButton />
      <Spinner className="size-3.5 text-muted-foreground" />
    </header>
  );
}

function InlineTitle({ threadId, title, editing, onEditingChange }: { threadId: string; title: string; editing: boolean; onEditingChange: (editing: boolean) => void }) {
  const t = useT();
  const settled = useRef(false);
  useEffect(() => {
    if (editing) settled.current = false;
  }, [editing]);
  const finish = (value: string | null) => {
    if (settled.current) return;
    settled.current = true;
    const next = value?.trim();
    if (next && next !== title) void actions.updateThread(threadId, { title: next });
    onEditingChange(false);
  };
  if (editing) {
    return (
      <input
        autoFocus
        defaultValue={title}
        aria-label={t("thread.renameTitle")}
        data-no-drag
        onFocus={(event) => event.currentTarget.select()}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            finish(event.currentTarget.value);
          } else if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            finish(null);
          }
        }}
        onBlur={(event) => finish(event.currentTarget.value)}
        className="-mx-1 h-6 min-w-0 flex-1 rounded-sm bg-transparent px-1 text-sm font-medium text-foreground outline-none ring-2 ring-ring/40"
      />
    );
  }
  return (
    <h1
      data-no-drag
      onDoubleClick={() => onEditingChange(true)}
      title={t("thread.renameDblClick")}
      className="min-w-0 cursor-default truncate text-sm font-medium text-foreground select-none"
    >
      {title}
    </h1>
  );
}

function useChildren(threadId: string) {
  const threads = useStore((s) => s.threads);
  return useMemo(() => Object.values(threads).filter((thread) => thread.parent?.threadId === threadId), [threads, threadId]);
}

function SubSessionsPill({ summary }: { summary: ThreadSummary }) {
  const t = useT();
  const children = useChildren(summary.id);
  const drawer = useStore((s) => s.rightDrawer);
  const count = Math.max(summary.childCount, children.length);
  if (count === 0) return null;
  const active =
    (drawer?.kind === "children" && drawer.parentId === summary.id) ||
    (drawer?.kind === "child" && children.some((child) => child.id === drawer.threadId));
  const running = children.some((child) => child.status === "running" || child.status === "awaiting_input");
  return (
    <Tip label={t("thread.subSessions", { n: count })}>
      <Button
        size="xs"
        variant="ghost-muted"
        aria-pressed={active}
        className={cn("tabular-nums", active && "bg-accent text-foreground")}
        onClick={() => {
          if (active) actions.closeDrawer();
          else if (count === 1 && children.length === 1) actions.openChildDrawer(children[0]!.id, summary.id);
          else actions.openChildrenDrawer(summary.id);
        }}
      >
        {running ? <Spinner className="size-3 text-info-foreground" /> : <CornerDownRightIcon />}
        {count}
      </Button>
    </Tip>
  );
}

function ChangesPill({ threadId, cwd }: { threadId: string; cwd: string | null }) {
  const t = useT();
  const status = useStore((s) => (cwd ? s.gitStatus[cwd] : undefined));
  const edited = useEditedFiles(threadId);
  const active = useStore((s) => s.rightDrawer?.kind === "changes");
  const git = status?.isRepo ? status.files : [];
  const fromGit = git.length > 0;
  const count = fromGit ? git.length : edited.length;
  if (count === 0) return null;
  const additions = fromGit ? git.reduce((sum, file) => sum + (file.additions ?? 0), 0) : edited.reduce((sum, file) => sum + file.additions, 0);
  const deletions = fromGit ? git.reduce((sum, file) => sum + (file.deletions ?? 0), 0) : edited.reduce((sum, file) => sum + file.deletions, 0);
  return (
    <Tip label={`${t("changes.pill", { n: count })} · ${changesShortcut}`}>
      <Button
        size="xs"
        variant="ghost-muted"
        aria-pressed={active}
        className={cn("tabular-nums", active && "bg-accent text-foreground")}
        onClick={actions.toggleChangesDrawer}
      >
        <FileDiffIcon />
        <span>{count}</span>
        {additions + deletions > 0 ? (
          <span className="font-mono text-[11px] max-sm:hidden">
            <span className="text-diff-addition-foreground">+{additions}</span> <span className="text-diff-deletion-foreground">−{deletions}</span>
          </span>
        ) : null}
      </Button>
    </Tip>
  );
}

function OpenMenu({ cwd }: { cwd: string }) {
  const t = useT();
  return (
    <Menu>
      <Tip label={t("open.menu")}>
        <MenuTrigger render={<Button size="icon-sm" variant="ghost" aria-label={t("open.menu")} />}>
          <SquareArrowOutUpRightIcon />
        </MenuTrigger>
      </Tip>
      <MenuPopup align="end" className="w-52">
        <MenuItem onClick={() => void actions.openExternal(cwd, "editor")}>
          <CodeIcon />
          {t("open.editor")}
        </MenuItem>
        <MenuItem onClick={() => void actions.openExternal(cwd, "terminal")}>
          <SquareTerminalIcon />
          {t("open.terminal")}
        </MenuItem>
        <MenuItem onClick={() => void actions.openExternal(cwd, "folder")}>
          <FolderOpenIcon />
          {t("open.folder")}
        </MenuItem>
      </MenuPopup>
    </Menu>
  );
}

function ThreadHeader({ thread, threadId }: { thread: OpenThread | undefined; threadId: string }) {
  const t = useT();
  const summary = useStore((s) => s.threads[threadId] ?? s.open[threadId]?.summary)!;
  const agent = useStore((s) => s.agents.find((a) => a.id === summary.agentId));
  const home = useStore((s) => s.app?.homeDir);
  const dataDir = useStore((s) => s.app?.dataDir);
  const parent = useStore((s) => (summary.parent ? s.threads[summary.parent.threadId] : undefined));
  const projectName = useStore((s) => {
    const cwd = summary.cwd?.replace(/[\\/]+$/, "").toLowerCase();
    return cwd ? s.projects.find((project) => project.path.replace(/[\\/]+$/, "").toLowerCase() === cwd)?.name : undefined;
  });
  const local = useIsLocalDesktop();
  const [editing, setEditing] = useState(false);
  const drag = useWindowDragProps();
  const chromeClass = useHeaderChromeClass("main");
  const cwd = summary.cwd?.trim() ? summary.cwd : null;
  const quickChat = isQuickChatPath(cwd, dataDir);
  const branch = useStore((s) => (cwd && !quickChat ? s.gitStatus[cwd]?.branch : null));
  const worktrees = useStore((s) => (cwd && !quickChat ? s.worktrees[cwd] : undefined));
  const currentWorktree = worktrees?.isRepo ? worktrees.worktrees.find((w) => w.isCurrent && !w.isMain) : undefined;
  const idle = summary.status === "idle" || summary.status === "error";
  const modelOption = thread?.options.find((option) => option.category === "model");
  const model = (modelOption && prepare(modelOption).find((choice) => choice.value === modelOption.currentValue)?.label) ?? summary.model;
  const source = summary.source !== "verre" ? t("thread.from", { source: sourceLabel(summary, agent?.name, t) }) : null;
  const projectLabel = cwd ? (quickChat ? t("sidebar.quickChats") : (projectName ?? basename(cwd))) : null;

  useEffect(() => {
    if (cwd && !quickChat && idle) {
      void actions.refreshGitStatus(cwd);
      void actions.refreshWorktrees(cwd);
    }
  }, [cwd, quickChat, idle]);

  const where = (
    <span className="flex flex-col gap-0.5 text-xs">
      <span className="flex items-center gap-1.5 font-medium">
        <span>{agent?.name ?? summary.agentId}</span>
        {model ? <span className="text-muted-foreground">· {model}</span> : null}
      </span>
      {branch ? (
        <span className="flex items-center gap-1 text-muted-foreground">
          <GitBranchIcon className="size-3 shrink-0" />
          <span className="font-mono text-[11px]">{branch}</span>
          {currentWorktree ? <span>· {t("worktree.badge")}</span> : null}
        </span>
      ) : null}
      {currentWorktree && worktrees ? (
        <span className="flex items-center gap-1 text-muted-foreground">
          <FolderGit2Icon className="size-3 shrink-0" />
          <span>{t("worktree.mainRepo", { path: tildePath(worktrees.mainRoot, home) })}</span>
        </span>
      ) : null}
      {cwd ? <span className="font-mono text-[11px] break-all text-muted-foreground">{tildePath(cwd, home)}</span> : null}
      {source ? <span className="text-muted-foreground">{source}</span> : null}
    </span>
  );

  return (
    <header {...drag} className={cn("flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 px-(--workspace-gutter)", chromeClass)}>
      <MobileSidebarButton />
      <div className="flex min-w-0 flex-1 items-center gap-1.5">
        <Tip label={where}>
          <button
            type="button"
            aria-label={agent?.name ?? summary.agentId}
            className="inline-flex min-w-0 shrink cursor-default items-center gap-1.5 rounded-sm text-sm text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
          >
            <AgentIcon agentId={summary.agentId} className="size-3.5 shrink-0" />
            {summary.live && !editing ? (
              <span className="size-1.5 shrink-0 rounded-full bg-success" aria-label={t("thread.live")} />
            ) : null}
            {projectLabel ? <span className="max-w-36 truncate">{projectLabel}</span> : null}
          </button>
        </Tip>
        {projectLabel ? <ChevronRightIcon className="size-3.5 shrink-0 text-muted-foreground/45" /> : null}
        {summary.parent ? (
          <>
            <button
              type="button"
              onClick={() => actions.navigate({ view: "thread", threadId: summary.parent!.threadId })}
              className="inline-flex min-w-0 max-w-44 shrink cursor-pointer items-center gap-1 rounded-sm text-sm text-muted-foreground hover:text-foreground"
              title={t("thread.openParent")}
            >
              {parent ? <AgentIcon agentId={parent.agentId} className="size-3.5 shrink-0" /> : <CornerLeftUpIcon className="size-3.5 shrink-0" />}
              <span className="truncate">{parent?.title ?? t("thread.parent")}</span>
            </button>
            <ChevronRightIcon className="size-3.5 shrink-0 text-muted-foreground/45" />
          </>
        ) : null}
        <div className="flex min-w-0 flex-1 items-center gap-1.5">
          <InlineTitle threadId={threadId} title={summary.title} editing={editing} onEditingChange={setEditing} />
          {summary.pinned && !editing ? <PinIcon className="size-3 shrink-0 text-muted-foreground/70" aria-label={t("thread.pinned")} /> : null}
        </div>
      </div>
      <div className="flex shrink-0 items-center gap-1">
        {!summary.resumable ? (
          <Tip label={t("thread.readOnlyHint", { agent: agent?.name ?? summary.agentId })}>
            <Badge variant="warning">
              <LockIcon />
              <span className="max-sm:hidden">{t("thread.readOnly")}</span>
            </Badge>
          </Tip>
        ) : null}
        <SubSessionsPill summary={summary} />
        <ChangesPill threadId={threadId} cwd={cwd} />
        {local && cwd ? <OpenMenu cwd={cwd} /> : null}
        <Menu>
          <MenuTrigger render={<Button size="icon-sm" variant="ghost" aria-label={t("thread.more")} />}>
            <MoreHorizontalIcon />
          </MenuTrigger>
          <MenuPopup align="end" className="w-64">
            <MenuItem onClick={() => setEditing(true)}>
              <PencilIcon />
              {t("thread.rename")}
            </MenuItem>
            <MenuItem onClick={() => void actions.updateThread(threadId, { pinned: !summary.pinned })}>
              {summary.pinned ? <PinOffIcon /> : <PinIcon />}
              {summary.pinned ? t("thread.unpin") : t("thread.pin")}
            </MenuItem>
            <MenuItem onClick={() => void actions.updateThread(threadId, { archived: !summary.archived })}>
              {summary.archived ? <ArchiveRestoreIcon /> : <ArchiveIcon />}
              {summary.archived ? t("thread.unarchive") : t("thread.archive")}
            </MenuItem>
            {agent?.capabilities.fork.whole && summary.resumable ? (
              <MenuItem disabled={!idle} onClick={() => void actions.forkThread(threadId)}>
                <GitForkIcon />
                {t("thread.fork")}
              </MenuItem>
            ) : null}
            <MenuItem onClick={actions.openRawDrawer}>
              <ScrollTextIcon />
              {t("thread.rawLog")}
              <kbd className="ms-auto font-sans text-xs text-muted-foreground">{rawLogShortcut}</kbd>
            </MenuItem>
            {parent ? (
              <MenuItem onClick={() => actions.navigate({ view: "thread", threadId: parent.id })}>
                <CornerLeftUpIcon />
                <span className="min-w-0 truncate">{t("thread.openParent")}</span>
              </MenuItem>
            ) : null}
            <MenuSeparator />
            <MenuItem onClick={() => void copyText(summary.nativeId).then(() => toast.success(t("common.copied")))}>
              <CopyIcon />
              {t("thread.copyId")}
            </MenuItem>
            {cwd ? (
              <MenuItem onClick={() => void copyText(cwd).then(() => toast.success(t("common.copied")))}>
                <CopyIcon />
                {t("open.copyPath")}
              </MenuItem>
            ) : null}
            <MenuItem
              disabled={!summary.live}
              onClick={() => void actions.detach(threadId).then((done) => done && toast.info(t("thread.detached")))}
            >
              <UnplugIcon />
              {t("thread.detach")}
            </MenuItem>
            {cwd || source ? (
              <>
                <MenuSeparator />
                <div className="flex flex-col gap-0.5 px-2 py-1.5 text-xs text-muted-foreground">
                  {source ? <span>{source}</span> : null}
                  {cwd ? <span className="font-mono text-[11px] break-all text-muted-foreground/80">{tildePath(cwd, home)}</span> : null}
                </div>
              </>
            ) : null}
          </MenuPopup>
        </Menu>
      </div>
    </header>
  );
}

function ThreadComposer({ thread }: { thread: OpenThread }) {
  const t = useT();
  const summary = useStore((s) => s.threads[thread.summary.id] ?? thread.summary);
  const agent = useStore((s) => s.agents.find((a) => a.id === summary.agentId));
  const dataDir = useStore((s) => s.app?.dataDir);
  const bypassMemoryScope = usePrefs((s) => s.bypassMemoryScope);
  const agentName = agent?.name ?? summary.agentId;
  const running = summary.status === "running" || summary.status === "awaiting_input";
  const [signIn, setSignIn] = useState(false);
  const needsSignIn = agent && (signIn || agent.auth.state === "required") && agent.auth.methods.length > 0;
  const realProjectCwd =
    summary.cwd && !isScratchOrTempPath(summary.cwd) && !isQuickChatPath(summary.cwd, dataDir) ? summary.cwd : null;
  return (
    <>
    {needsSignIn ? <SignInCard agent={agent} onDone={() => setSignIn(false)} /> : null}
    <Composer
      draftKey={summary.id}
      agentName={agentName}
      cwd={summary.cwd}
      options={thread.options}
      commands={thread.commands}
      queue={thread.queue}
      onCancelQueued={(queueId) => void actions.cancelQueued(summary.id, queueId)}
      onOptionChange={(optionId, value) => {
        const option = thread.options.find((entry) => entry.id === optionId);
        void actions.setOption(summary.id, optionId, value).then((applied) => {
          if (!option || !applied) return;
          const activeModel =
            option.category === "model" ? value : thread.options.find((entry) => entry.category === "model")?.currentValue;
          rememberAgentOption(summary.agentId, realProjectCwd, option, value, undefined, activeModel, bypassMemoryScope);
        });
      }}
      onSubmit={(content) =>
        actions.send(summary.id, content).catch((error: unknown) => {
          if (needsAuth(error)) setSignIn(true);
          throw error;
        })
      }
      onStop={() => void actions.interrupt(summary.id)}
      running={running}
      midTurnInput={agent?.capabilities.midTurnInput}
      imageInput={agent?.capabilities.imageInput ?? false}
      readOnly={!summary.resumable}
      placeholder={summary.resumable ? t("composer.placeholder", { agent: agentName }) : t("composer.readOnly")}
      usage={thread.usage}
      agent={agent}
      autoFocus
    />
    </>
  );
}

function needsAuth(error: unknown) {
  return error instanceof RpcFailure && (error.code === "auth_required" || /authenticat/i.test(error.message));
}
