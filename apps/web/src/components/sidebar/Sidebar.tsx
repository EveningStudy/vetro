import {
  ArchiveIcon,
  ArchiveRestoreIcon,
  ChevronDownIcon,
  CircleAlertIcon,
  CodeIcon,
  CopyIcon,
  CornerDownRightIcon,
  EyeIcon,
  EyeOffIcon,
  FolderIcon,
  FolderMinusIcon,
  FolderOpenIcon,
  FolderXIcon,
  ListFilterIcon,
  LoaderCircleIcon,
  MessageCircleQuestionIcon,
  MoreHorizontalIcon,
  PanelLeftCloseIcon,
  PencilIcon,
  PinIcon,
  PinOffIcon,
  ScrollTextIcon,
  SearchIcon,
  SettingsIcon,
  SquarePenIcon,
  SquareTerminalIcon,
  UnplugIcon,
} from "lucide-react";
import { createContext, memo, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import type { Project, ThreadSource, ThreadStatus, ThreadSummary } from "~/api/types";
import { useT } from "~/i18n";
import { relativeTime } from "~/lib/format";
import { cn, copyText, isMac } from "~/lib/utils";
import { RpcFailure } from "~/api/protocol";
import { type SidebarScope, setSidebarScope, usePrefs } from "~/state/prefs";
import { actions, useIsLocalDesktop, useStore } from "~/state/store";
import { errorMessage, toast } from "~/state/toast";
import { AgentIcon } from "../AgentIcon";
import { useNow } from "../hooks";
import { RenameDialog } from "../RenameDialog";
import { Button } from "../ui/button";
import {
  ContextMenu,
  Dialog,
  DialogDescription,
  DialogPopup,
  DialogTitle,
  Menu,
  MenuCheckboxItem,
  MenuGroupLabel,
  MenuItem,
  MenuItemHint,
  MenuPopup,
  MenuSeparator,
  MenuTrigger,
  Tip,
} from "../ui/overlay";
import { Kbd } from "../ui/primitives";
import { VetroMark } from "../VetroMark";

const SOURCES: ThreadSource[] = ["vetro", "desktop", "cli", "exec", "ide", "cloud", "unknown"];
const PAGE = 5;
const QUICK_KEY = "\u0000quick";

const rowBase =
  "group/sidebar-row relative flex w-full cursor-pointer select-none items-center gap-2.5 overflow-hidden rounded-md text-left outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring";

function SidebarAction({
  icon: Icon,
  label,
  onClick,
  trailing,
  active,
}: {
  icon: typeof SearchIcon;
  label: string;
  onClick: () => void;
  trailing?: React.ReactNode;
  active?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        rowBase,
        "h-8 px-(--sidebar-row-content-inset) text-sm text-sidebar-foreground/90 hover:bg-sidebar-row-hover",
        active && "bg-sidebar-row-active text-sidebar-foreground light:ring-1 light:ring-inset light:ring-sidebar-border/70",
      )}
    >
      <Icon className="size-4 shrink-0 text-sidebar-muted-foreground" />
      <span className="min-w-0 flex-1 truncate font-medium">{label}</span>
      {trailing}
    </button>
  );
}

function StatusLabel({ thread, iconOnly }: { thread: ThreadSummary; iconOnly?: boolean }) {
  const t = useT();
  const status =
    thread.status === "running"
      ? { Icon: LoaderCircleIcon, className: "text-info-foreground", spin: true }
      : thread.status === "awaiting_input"
        ? { Icon: MessageCircleQuestionIcon, className: "text-warning-foreground", spin: false }
        : thread.status === "error"
          ? { Icon: CircleAlertIcon, className: "text-error-foreground", spin: false }
          : null;
  if (!status || thread.status === "idle") return null;
  const label = t(`status.${thread.status}`);
  return (
    <span className={cn("inline-flex items-center gap-1 text-xs font-medium", status.className)} title={iconOnly ? label : undefined}>
      <status.Icon className={cn(iconOnly ? "size-3.5" : "size-3", status.spin && "animate-spin")} aria-label={iconOnly ? label : undefined} />
      {iconOnly ? null : label}
    </span>
  );
}

const ROW_HEIGHT = { project: 40, thread: 37, more: 29, empty: 29, loadall: 37, section: 40, heading: 32 } as const;

type Heading = "active" | "quick" | "projects";

type SidebarRow =
  | { kind: "project"; key: string; project: Pick<Project, "path" | "name" | "exists" | "pinned" | "hidden">; suffix?: string; count: number; collapsed: boolean; attention: string | null }
  | { kind: "thread"; key: string; thread: ThreadSummary; context?: string }
  | { kind: "more"; key: string; path: string; hidden: number; remote: number; expanded: boolean; flat?: boolean }
  | { kind: "section"; key: string; count: number; open: boolean }
  | { kind: "heading"; key: string; heading: Heading; count: number }
  | { kind: "loadall"; key: string }
  | { kind: "empty"; key: string; text: string };

const ACTIVE_STATUSES = new Set<ThreadStatus>(["running", "awaiting_input", "error"]);
const URGENCY: Partial<Record<ThreadStatus, number>> = { awaiting_input: 0, error: 1, running: 2 };
const DAY = 86_400_000;

/** Throwaway checkouts and temp folders: kept out of the main list unless something is happening there. */
export function isScratchOrTempPath(path: string) {
  const p = path.replace(/^\\\\\?\\/, "").replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
  const base = p.split("/").at(-1) ?? "";
  if (base.startsWith("tmp.") || base.startsWith("scratch-") || base.startsWith("temp-")) return true;
  return (
    /(^|\/)\.orchestra(\/|$)/.test(p) ||
    /(^|\/)\.vetro(\/|$)/.test(p) ||
    /(^|\/)temp(\/|$)/.test(p) ||
    /^(\/private)?\/tmp(\/|$)/.test(p) ||
    /^\/var\/folders\//.test(p)
  );
}

const QUICK_CHAT_PATHS = [
  /(?:^|[\\/])Documents[\\/]Codex[\\/]\d{4}-\d{2}-\d{2}(?:[\\/]|$)/i,
  /(?:^|[\\/])\.codex[\\/]worktrees[\\/]/i,
  /(?:^|[\\/])Claude-Data[\\/].*[\\/]scratch-workspaces[\\/]/i,
];

/** Folders an agent's desktop app creates on its own for a one-off chat. Their threads are listed
 * flat under Quick chats instead of as one-thread projects. */
export function isQuickChatPath(path: string | null) {
  if (!path || !path.trim()) return true;
  return QUICK_CHAT_PATHS.some((pattern) => pattern.test(path));
}

function basename(path: string | null) {
  return (path ?? "").split(/[\\/]/).filter(Boolean).at(-1) ?? "";
}

function byPinThenRecent(a: ThreadSummary, b: ThreadSummary) {
  return Number(b.pinned) - Number(a.pinned) || b.updatedAt - a.updatedAt;
}

const RenameContext = createContext<(thread: ThreadSummary) => void>(() => undefined);

function copy(text: string, t: ReturnType<typeof useT>) {
  void copyText(text).then((ok) => (ok ? toast.success(t("common.copied")) : undefined));
}

function ThreadMenuItems({ thread }: { thread: ThreadSummary }) {
  const t = useT();
  const local = useIsLocalDesktop();
  const rename = useContext(RenameContext);
  const cwd = thread.cwd?.trim() ? thread.cwd : null;
  return (
    <>
      <MenuItem onClick={() => rename(thread)}>
        <PencilIcon />
        {t("thread.rename")}
      </MenuItem>
      <MenuItem onClick={() => void actions.updateThread(thread.id, { pinned: !thread.pinned })}>
        {thread.pinned ? <PinOffIcon /> : <PinIcon />}
        {thread.pinned ? t("thread.unpin") : t("thread.pin")}
      </MenuItem>
      <MenuItem onClick={() => void actions.updateThread(thread.id, { archived: !thread.archived })} className="items-start py-1.5">
        {thread.archived ? <ArchiveRestoreIcon className="mt-0.5" /> : <ArchiveIcon className="mt-0.5" />}
        <span className="min-w-0">
          {thread.archived ? t("thread.unarchive") : t("thread.archive")}
          <MenuItemHint>{t("thread.archiveHint")}</MenuItemHint>
        </span>
      </MenuItem>
      {cwd ? (
        <>
          <MenuSeparator />
          <MenuItem onClick={() => actions.navigate({ view: "new", projectPath: cwd, agentId: thread.agentId })}>
            <SquarePenIcon />
            <span className="min-w-0 truncate">{t("sidebar.newThreadIn", { name: basename(cwd) })}</span>
          </MenuItem>
          {local ? (
            <MenuItem onClick={() => void actions.openExternal(cwd, "folder")}>
              <FolderOpenIcon />
              {t("open.folder")}
            </MenuItem>
          ) : null}
        </>
      ) : null}
      <MenuSeparator />
      <MenuItem onClick={() => copy(thread.nativeId, t)}>
        <CopyIcon />
        {t("thread.copyId")}
      </MenuItem>
      {cwd ? (
        <MenuItem onClick={() => copy(cwd, t)}>
          <CopyIcon />
          {t("open.copyPath")}
        </MenuItem>
      ) : null}
      <MenuItem
        onClick={() => {
          actions.navigate({ view: "thread", threadId: thread.id });
          actions.openRawDrawer();
        }}
      >
        <ScrollTextIcon />
        {t("thread.rawLog")}
      </MenuItem>
      {thread.live ? (
        <MenuItem onClick={() => void actions.detach(thread.id).then(() => toast.info(t("thread.detached")))}>
          <UnplugIcon />
          {t("thread.detach")}
        </MenuItem>
      ) : null}
    </>
  );
}

const ThreadRow = memo(function ThreadRow({
  thread,
  active,
  now,
  context,
}: {
  thread: ThreadSummary;
  active: boolean;
  now: number;
  context?: string;
}) {
  const t = useT();
  const busy = thread.status !== "idle";
  const unread = useStore((s) => !!s.unreadCompleted[thread.id]);
  return (
    <ContextMenu content={() => <ThreadMenuItems thread={thread} />}>
      <button
        type="button"
        aria-current={active ? "page" : undefined}
        onClick={() => actions.navigate({ view: "thread", threadId: thread.id })}
        title={context ? `${thread.title} · ${thread.cwd ?? context}` : undefined}
        className={cn(
          rowBase,
          "h-9 ps-3 pe-2.5",
          active
            ? "bg-sidebar-row-active text-sidebar-foreground light:ring-1 light:ring-inset light:ring-sidebar-border/70"
            : "text-sidebar-foreground hover:bg-sidebar-row-hover data-popup-open:bg-sidebar-row-hover",
        )}
      >
        <span
          className={cn(
            "flex shrink-0 items-center transition-[opacity,filter]",
            !active && !busy && !unread && "opacity-60 grayscale group-hover/sidebar-row:opacity-100 group-hover/sidebar-row:grayscale-0",
          )}
        >
          <AgentIcon agentId={thread.agentId} className="size-4" />
        </span>
        <span
          className={cn(
            "min-w-0 flex-1 truncate text-start text-sm",
            active || busy || unread ? "font-medium text-sidebar-foreground" : "text-sidebar-foreground/80 group-hover/sidebar-row:text-sidebar-foreground",
            thread.archived && "italic",
          )}
        >
          {thread.title}
        </span>
        {context ? <span className="max-w-[35%] shrink truncate text-xs text-sidebar-muted-foreground">{context}</span> : null}
        {thread.pinned ? <PinIcon className="size-3 shrink-0 text-sidebar-muted-foreground/80" aria-label={t("thread.pinned")} /> : null}
        {thread.childCount > 0 ? (
          <span
            className="inline-flex shrink-0 items-center gap-0.5 text-[11px] tabular-nums text-sidebar-muted-foreground/80"
            title={t("sidebar.children", { n: thread.childCount })}
          >
            <CornerDownRightIcon className="size-3" />
            {thread.childCount}
          </span>
        ) : null}
        {thread.archived ? <ArchiveIcon className="size-3 shrink-0 text-sidebar-muted-foreground" /> : null}
        <span className="ml-auto flex h-6 shrink-0 items-center justify-end gap-1.5 tabular-nums text-secondary-label text-xs">
          {busy ? (
            <StatusLabel thread={thread} iconOnly={!!context} />
          ) : (
            <>
              {unread ? <span className="size-1.5 rounded-full bg-info" aria-label={t("sidebar.unread")} /> : null}
              {relativeTime(thread.updatedAt, t, now)}
            </>
          )}
        </span>
      </button>
    </ContextMenu>
  );
});

function ProjectMenuItems({ project, onRemove }: { project: Pick<Project, "path" | "name" | "exists" | "pinned" | "hidden">; onRemove: () => void }) {
  const t = useT();
  const local = useIsLocalDesktop();
  return (
    <>
      <MenuItem onClick={() => actions.navigate({ view: "new", projectPath: project.path, agentId: null })}>
        <SquarePenIcon />
        {t("sidebar.newThread")}
      </MenuItem>
      <MenuItem onClick={() => void actions.updateProject(project.path, { pinned: !project.pinned })}>
        {project.pinned ? <PinOffIcon /> : <PinIcon />}
        {project.pinned ? t("sidebar.unpin") : t("sidebar.pin")}
      </MenuItem>
      <MenuItem onClick={() => void actions.updateProject(project.path, { hidden: !project.hidden })}>
        {project.hidden ? <EyeIcon /> : <EyeOffIcon />}
        {project.hidden ? t("sidebar.unhide") : t("sidebar.hide")}
      </MenuItem>
      {local && project.exists ? (
        <>
          <MenuSeparator />
          <MenuItem onClick={() => void actions.openExternal(project.path, "folder")}>
            <FolderOpenIcon />
            {t("open.folder")}
          </MenuItem>
          <MenuItem onClick={() => void actions.openExternal(project.path, "terminal")}>
            <SquareTerminalIcon />
            {t("open.terminal")}
          </MenuItem>
          <MenuItem onClick={() => void actions.openExternal(project.path, "editor")}>
            <CodeIcon />
            {t("open.editor")}
          </MenuItem>
        </>
      ) : null}
      <MenuSeparator />
      <MenuItem onClick={() => copy(project.path, t)}>
        <CopyIcon />
        {t("open.copyPath")}
      </MenuItem>
      <MenuItem destructive onClick={onRemove}>
        <FolderMinusIcon />
        {t("sidebar.remove")}
      </MenuItem>
    </>
  );
}

const ProjectHeader = memo(function ProjectHeader({ row }: { row: Extract<SidebarRow, { kind: "project" }> }) {
  const t = useT();
  const { project, collapsed, count, attention, suffix } = row;
  const [refusal, setRefusal] = useState<string | null>(null);
  const real = project.path !== "";
  const remove = () =>
    actions
      .removeProject(project.path)
      .then((removed) => (removed ? toast.success(t("sidebar.removed", { name: project.name })) : setRefusal(t("sidebar.removeKept"))))
      .catch((error: unknown) => {
        if (error instanceof RpcFailure && error.code === "bad_request") setRefusal(error.message);
        else if (error instanceof RpcFailure && /unknown rpc method/i.test(error.message)) setRefusal(t("sidebar.removeUnsupported"));
        else toast.error(errorMessage(error));
      });
  return (
    <div className="group/project-folder relative pt-2">
      <ContextMenu disabled={!real} content={() => <ProjectMenuItems project={project} onRemove={() => void remove()} />}>
        <button
          type="button"
          aria-expanded={!collapsed}
          onClick={() => actions.toggleProject(project.path, collapsed)}
          title={project.path || undefined}
          className="flex h-8 w-full cursor-pointer items-center gap-2.5 rounded-(--control-radius) px-(--sidebar-row-content-inset) text-left outline-none hover:bg-sidebar-row-hover focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring data-popup-open:bg-sidebar-row-hover"
        >
          {project.exists ? (
            <FolderIcon className="size-4 shrink-0 text-sidebar-muted-foreground" />
          ) : (
            <FolderXIcon className="size-4 shrink-0 text-sidebar-muted-foreground/70" />
          )}
          <span className={cn("min-w-0 flex-1 truncate text-sm font-medium text-sidebar-foreground/90", (!project.exists || project.hidden) && "text-sidebar-muted-foreground")}>
            {project.name}
            {suffix ? <span className="ms-1.5 font-normal text-xs text-sidebar-muted-foreground">{suffix}</span> : null}
          </span>
          {project.pinned ? <PinIcon className="size-3 shrink-0 text-sidebar-muted-foreground/70" /> : null}
          {project.hidden ? <EyeOffIcon className="size-3 shrink-0 text-sidebar-muted-foreground" /> : null}
          {attention ? <span className={cn("size-1.5 shrink-0 rounded-full", attention)} /> : null}
          {collapsed && count > 0 ? (
            <span className="shrink-0 text-xs tabular-nums text-sidebar-muted-foreground group-hover/project-folder:invisible pointer-coarse:invisible">{count}</span>
          ) : null}
          <ChevronDownIcon className={cn("size-3 shrink-0 text-sidebar-muted-foreground transition-transform", collapsed && "-rotate-90")} />
        </button>
      </ContextMenu>
      {real ? (
        <span className="absolute top-3 right-6 flex items-center gap-0.5 opacity-0 focus-within:opacity-100 group-hover/project-folder:opacity-100 pointer-coarse:opacity-100">
          <Menu>
            <MenuTrigger render={<Button size="icon-xs" variant="ghost-muted" aria-label={t("thread.more")} />}>
              <MoreHorizontalIcon />
            </MenuTrigger>
            <MenuPopup align="end">
              <ProjectMenuItems project={project} onRemove={() => void remove()} />
            </MenuPopup>
          </Menu>
          <Tip label={t("sidebar.newThreadIn", { name: project.name })}>
            <Button
              size="icon-xs"
              variant="ghost-muted"
              aria-label={t("sidebar.newThreadIn", { name: project.name })}
              onClick={() => actions.navigate({ view: "new", projectPath: project.path, agentId: null })}
            >
              <SquarePenIcon />
            </Button>
          </Tip>
        </span>
      ) : null}
      <Dialog open={refusal !== null} onOpenChange={(open) => !open && setRefusal(null)}>
        <DialogPopup className="max-w-md">
          <div className="flex flex-col gap-4 p-5">
            <div className="flex flex-col gap-1 pe-8">
              <DialogTitle>{t("sidebar.removeRefusedTitle", { name: project.name })}</DialogTitle>
              <DialogDescription>{refusal}</DialogDescription>
            </div>
            <div className="flex justify-end gap-2">
              <Button variant="ghost" onClick={() => setRefusal(null)}>
                {t("common.cancel")}
              </Button>
              <Button
                onClick={() => {
                  setRefusal(null);
                  void actions.updateProject(project.path, { hidden: true });
                }}
              >
                <EyeOffIcon />
                {t("sidebar.hide")}
              </Button>
            </div>
          </div>
        </DialogPopup>
      </Dialog>
    </div>
  );
});

function ScopeToggle() {
  const t = useT();
  const scope = usePrefs((s) => s.sidebarScope);
  const options: { value: SidebarScope; label: string }[] = [
    { value: "all", label: t("sidebar.scopeAll") },
    { value: "vetro", label: t("sidebar.scopeVetro") },
  ];
  return (
    <div role="radiogroup" aria-label={t("settings.sidebarScope")} className="flex shrink-0 rounded-md bg-sidebar-row-hover p-px">
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          role="radio"
          aria-checked={scope === option.value}
          onClick={() => setSidebarScope(option.value)}
          className={cn(
            "h-5 rounded-[5px] px-1.5 text-[11px] font-medium leading-none text-sidebar-muted-foreground outline-none hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-ring",
            scope === option.value && "bg-sidebar text-sidebar-foreground shadow-xs dark:bg-sidebar-row-active",
          )}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

function HeadingRow({ row }: { row: Extract<SidebarRow, { kind: "heading" }> }) {
  const t = useT();
  const label = row.heading === "active" ? t("sidebar.active") : row.heading === "quick" ? t("sidebar.quickChats") : t("sidebar.projects");
  return (
    <div className="flex h-full items-end gap-2 px-(--sidebar-row-content-inset) pb-1 text-xs font-medium text-sidebar-muted-foreground">
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {row.heading !== "projects" && row.count > 0 ? <span className="tabular-nums text-sidebar-muted-foreground/70">{row.count}</span> : null}
      {row.heading === "projects" ? <ScopeToggle /> : null}
    </div>
  );
}

/** The sidebar can hold hundreds of projects and threads; only the rows near the viewport are
 * rendered. Every row kind has a fixed height, so offsets are exact. */
function VirtualRows({
  rows,
  activeId,
  now,
  onToggleMore,
  onToggleSection,
}: {
  rows: SidebarRow[];
  activeId: string | null;
  now: number;
  onToggleMore: (path: string) => void;
  onToggleSection: () => void;
}) {
  const scrollerRef = useRef<HTMLDivElement>(null);
  const [view, setView] = useState({ top: 0, height: 800 });
  const offsets = useMemo(() => {
    const list = [0];
    for (const row of rows) list.push(list.at(-1)! + rowHeight(row));
    return list;
  }, [rows]);

  useEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller) return;
    const update = () => setView({ top: scroller.scrollTop, height: scroller.clientHeight });
    const observer = new ResizeObserver(update);
    observer.observe(scroller);
    update();
    return () => observer.disconnect();
  }, []);

  const overscan = 600;
  const start = Math.max(0, upperBound(offsets, view.top - overscan) - 1);
  const end = Math.min(rows.length, upperBound(offsets, view.top + view.height + overscan));
  return (
    <div
      ref={scrollerRef}
      onScroll={(event) => setView({ top: event.currentTarget.scrollTop, height: event.currentTarget.clientHeight })}
      className="min-h-0 flex-1 overflow-y-auto px-2 pb-2 [scrollbar-gutter:stable]"
    >
      <div style={{ height: offsets.at(-1), position: "relative" }}>
        <div style={{ transform: `translateY(${offsets[start]}px)` }}>
          {rows.slice(start, end).map((row) => (
            <div key={row.key} style={{ height: rowHeight(row) }} className={row.kind === "thread" ? "pb-px" : undefined}>
              {row.kind === "project" ? (
                <ProjectHeader row={row} />
              ) : row.kind === "thread" ? (
                <ThreadRow thread={row.thread} active={row.thread.id === activeId} now={now} context={row.context} />
              ) : row.kind === "more" ? (
                <MoreRow row={row} onToggle={onToggleMore} />
              ) : row.kind === "section" ? (
                <SectionRow row={row} onToggle={onToggleSection} />
              ) : row.kind === "heading" ? (
                <HeadingRow row={row} />
              ) : row.kind === "loadall" ? (
                <LoadAllRow />
              ) : (
                <div className="flex h-full items-center ps-9 text-xs text-sidebar-muted-foreground">{row.text}</div>
              )}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

function LoadAllRow() {
  const t = useT();
  const loading = useStore((s) => s.loadingMore);
  return (
    <button
      type="button"
      disabled={loading}
      onClick={() => void actions.loadMoreThreads()}
      className="mt-2 h-8 w-full rounded-md px-(--sidebar-row-content-inset) text-left text-xs text-sidebar-muted-foreground hover:bg-sidebar-row-hover hover:text-sidebar-foreground disabled:opacity-60"
    >
      {loading ? t("common.loading") : t("sidebar.loadOlder")}
    </button>
  );
}

function MoreRow({ row, onToggle }: { row: Extract<SidebarRow, { kind: "more" }>; onToggle: (path: string) => void }) {
  const t = useT();
  return (
    <button
      type="button"
      onClick={() => {
        if (!row.expanded && row.hidden > 0) onToggle(row.path);
        else if (row.remote > 0) {
          if (!row.expanded) onToggle(row.path);
          void actions.loadProjectThreads(row.path);
        } else onToggle(row.path);
      }}
      className={cn(
        "h-7 w-full rounded-md text-left text-xs text-sidebar-muted-foreground hover:bg-sidebar-row-hover hover:text-sidebar-foreground",
        row.flat ? "ps-3" : "ps-9",
      )}
    >
      {!row.expanded && row.hidden > 0
        ? t("sidebar.showMore", { n: row.hidden + row.remote })
        : row.remote > 0
          ? t("sidebar.loadMore", { n: row.remote })
          : t("sidebar.showLess")}
    </button>
  );
}

function SectionRow({ row, onToggle }: { row: Extract<SidebarRow, { kind: "section" }>; onToggle: () => void }) {
  const t = useT();
  return (
    <div className="pt-2">
      <button
        type="button"
        aria-expanded={row.open}
        onClick={onToggle}
        className="flex h-8 w-full cursor-pointer items-center gap-2 rounded-(--control-radius) px-(--sidebar-row-content-inset) text-left text-xs font-medium text-sidebar-muted-foreground outline-none hover:bg-sidebar-row-hover hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
      >
        <ChevronDownIcon className={cn("size-3 shrink-0 transition-transform", !row.open && "-rotate-90")} />
        <span className="min-w-0 flex-1 truncate">{t("sidebar.otherFolders", { n: row.count })}</span>
      </button>
    </div>
  );
}

function rowHeight(row: SidebarRow) {
  return ROW_HEIGHT[row.kind];
}

function upperBound(offsets: number[], value: number) {
  let low = 0;
  let high = offsets.length;
  while (low < high) {
    const middle = (low + high) >> 1;
    if (offsets[middle]! <= value) low = middle + 1;
    else high = middle;
  }
  return low;
}

function FilterMenu() {
  const t = useT();
  const filters = useStore((s) => s.filters);
  const agents = useStore((s) => s.agents);
  const hiddenCount = useStore((s) => s.projects.filter((p) => p.hidden).length);
  const active = filters.agentIds.length + filters.sources.length + (filters.showArchived ? 1 : 0) + (filters.showHidden ? 1 : 0) + (filters.showInternal ? 1 : 0);
  const toggle = <T extends string>(list: T[], value: T) => (list.includes(value) ? list.filter((v) => v !== value) : [...list, value]);
  return (
    <Menu>
      <Tip label={t("sidebar.filter")}>
        <MenuTrigger
          render={
            <Button
              size="icon-xs"
              variant="ghost-muted"
              aria-label={t("sidebar.filter")}
              className={cn(active > 0 && "text-primary [--control-icon-color:var(--primary)]")}
            />
          }
        >
          <ListFilterIcon />
        </MenuTrigger>
      </Tip>
      <MenuPopup align="end" className="w-56">
        <MenuGroupLabel>{t("sidebar.filterAgents")}</MenuGroupLabel>
        {agents.map((agent) => (
          <MenuCheckboxItem
            key={agent.id}
            checked={filters.agentIds.includes(agent.id)}
            onCheckedChange={() => actions.setFilters({ agentIds: toggle(filters.agentIds, agent.id) })}
            closeOnClick={false}
          >
            <AgentIcon agentId={agent.id} className="size-3.5" />
            {agent.name}
          </MenuCheckboxItem>
        ))}
        <MenuSeparator />
        <MenuCheckboxItem
          checked={filters.showInternal}
          onCheckedChange={(checked) => actions.setFilters({ showInternal: checked })}
          closeOnClick={false}
        >
          {t("sidebar.showInternal")}
        </MenuCheckboxItem>
        <MenuCheckboxItem
          checked={filters.showArchived}
          onCheckedChange={(checked) => actions.setFilters({ showArchived: checked })}
          closeOnClick={false}
        >
          {t("sidebar.showArchived")}
        </MenuCheckboxItem>
        <MenuCheckboxItem
          checked={filters.showHidden}
          onCheckedChange={(checked) => actions.setFilters({ showHidden: checked })}
          closeOnClick={false}
        >
          {t("sidebar.showHidden")}
          {hiddenCount > 0 ? <span className="ms-auto text-xs tabular-nums text-muted-foreground">{hiddenCount}</span> : null}
        </MenuCheckboxItem>
        <MenuSeparator />
        <MenuGroupLabel>{t("sidebar.filterSources")}</MenuGroupLabel>
        <div className="grid grid-cols-2">
          {SOURCES.map((source) => (
            <MenuCheckboxItem
              key={source}
              checked={filters.sources.includes(source)}
              onCheckedChange={() => actions.setFilters({ sources: toggle(filters.sources, source) })}
              closeOnClick={false}
            >
              {t(`source.${source}`)}
            </MenuCheckboxItem>
          ))}
        </div>
        {active > 0 ? (
          <MenuItem onClick={() => actions.setFilters({ agentIds: [], sources: [], showArchived: false, showHidden: false, showInternal: false })}>
            {t("sidebar.clearFilters")}
          </MenuItem>
        ) : null}
      </MenuPopup>
    </Menu>
  );
}

function normalize(path: string | null) {
  return (path ?? "").replace(/[\\/]+$/, "").toLowerCase();
}

export function SidebarContent() {
  const t = useT();
  const now = useNow(30_000);
  const projects = useStore((s) => s.projects);
  const threads = useStore((s) => s.threads);
  const filters = useStore((s) => s.filters);
  const settings = useStore((s) => s.settings);
  const route = useStore((s) => s.route);
  const connection = useStore((s) => s.connection);
  const activeId = route.view === "thread" ? route.threadId : null;
  const openSummary = useStore((s) => (activeId ? s.open[activeId]?.summary : undefined));
  const activityDays = usePrefs((s) => s.sidebarActivityDays);
  const hideDisabled = usePrefs((s) => s.hideDisabledAgentHistory);
  const scope = usePrefs((s) => s.sidebarScope);
  const minute = Math.floor(now / 60_000);
  const [renaming, setRenaming] = useState<ThreadSummary | null>(null);

  const groups = useMemo(() => {
    const all = Object.values(threads)
      .filter((thread) => !thread.parent)
      .sort((a, b) => b.updatedAt - a.updatedAt);
    const disabled = (agentId: string) =>
      hideDisabled && settings?.agents[agentId]?.enabled === false && !filters.agentIds.includes(agentId);
    const matches = (thread: ThreadSummary) =>
      (scope === "all" || thread.source === "vetro") &&
      (filters.showArchived || !thread.archived) &&
      (filters.agentIds.length === 0 || filters.agentIds.includes(thread.agentId)) &&
      (filters.sources.length === 0 || filters.sources.includes(thread.source)) &&
      (!thread.internal || filters.showInternal || filters.sources.includes("exec")) &&
      !disabled(thread.agentId);
    const loaded = new Map<string, number>();
    for (const thread of all) loaded.set(normalize(thread.cwd), (loaded.get(normalize(thread.cwd)) ?? 0) + 1);
    const byProject = new Map<string, ThreadSummary[]>();
    for (const thread of all) {
      if (!matches(thread)) continue;
      const key = normalize(thread.cwd);
      const list = byProject.get(key);
      if (list) list.push(thread);
      else byProject.set(key, [thread]);
    }
    for (const list of byProject.values()) list.sort(byPinThenRecent);
    const hiddenPaths = new Set(projects.filter((project) => project.hidden).map((project) => normalize(project.path)));
    const pinnedPaths = new Set(projects.filter((project) => project.pinned).map((project) => normalize(project.path)));
    const quickKey = (key: string) => !pinnedPaths.has(key) && isQuickChatPath(key);
    const visiblePath = (key: string) => filters.showHidden || !hiddenPaths.has(key);

    const attention = [...byProject.entries()]
      .filter(([key]) => visiblePath(key))
      .flatMap(([, list]) => list)
      .filter((thread) => ACTIVE_STATUSES.has(thread.status))
      .sort((a, b) => (URGENCY[a.status] ?? 9) - (URGENCY[b.status] ?? 9) || b.updatedAt - a.updatedAt);

    const quick = [...byProject.entries()]
      .filter(([key]) => quickKey(key) && visiblePath(key))
      .flatMap(([, list]) => list)
      .sort(byPinThenRecent);

    const shown = projects.filter(
      (project) =>
        (filters.showHidden || !project.hidden) &&
        !quickKey(normalize(project.path)) &&
        (scope === "all" || project.pinned || project.threadCount === 0 || byProject.has(normalize(project.path))),
    );
    const known = new Set(projects.map((project) => normalize(project.path)));
    const other = [...byProject.entries()]
      .filter(([key]) => !known.has(key) && !quickKey(key))
      .flatMap(([, list]) => list)
      .sort(byPinThenRecent);

    const nameCounts = new Map<string, number>();
    for (const project of shown) nameCounts.set(project.name.toLowerCase(), (nameCounts.get(project.name.toLowerCase()) ?? 0) + 1);
    const suffixes = new Map<string, string>();
    for (const project of shown) {
      if ((nameCounts.get(project.name.toLowerCase()) ?? 0) < 2) continue;
      const parts = project.path.split(/[\\/]/).filter(Boolean);
      suffixes.set(project.path, parts.at(-2) ?? "");
    }

    const activeThread = activeId ? (threads[activeId] ?? openSummary) : undefined;
    const activeKey = activeThread ? normalize(activeThread.cwd) : null;
    const cutoff = activityDays === 0 ? null : minute * 60_000 - activityDays * DAY;
    const workspaces: ProjectEntry[] = [];
    const older: ProjectEntry[] = [];
    for (const project of shown) {
      const key = normalize(project.path);
      const list = byProject.get(key) ?? [];
      const newest = list.reduce<number | null>((max, thread) => Math.max(max ?? 0, thread.updatedAt), null);
      const latest = newest ?? (project.threadCount === 0 ? project.lastActivityAt : project.lastRootActivityAt);
      const busy = list.some((thread) => ACTIVE_STATUSES.has(thread.status));
      const containsActive = key === activeKey || list.some((thread) => thread.id === activeId);
      const recent =
        project.exists &&
        !isScratchOrTempPath(project.path) &&
        (list.length > 0 || project.threadCount === 0) &&
        (cutoff === null || latest === null || latest >= cutoff);
      const entry = { project, key, list, latest: latest ?? 0, containsActive };
      if (project.pinned || busy || containsActive || recent) workspaces.push(entry);
      else older.push(entry);
    }
    workspaces.sort((a, b) => Number(b.project.pinned) - Number(a.project.pinned) || b.latest - a.latest);
    older.sort((a, b) => b.latest - a.latest);
    return { workspaces, older, other, quick, attention, suffixes, loaded };
  }, [threads, projects, filters, settings, hideDisabled, activityDays, minute, activeId, openSummary, scope]);

  const filtering = filters.agentIds.length > 0 || filters.sources.length > 0;
  const scoped = scope === "vetro";
  const projectExpanded = useStore((s) => s.projectExpanded);
  const projectCursors = useStore((s) => s.projectCursors);
  const threadsCursor = useStore((s) => s.threadsCursor);
  const [expandedMore, setExpandedMore] = useState<Record<string, boolean>>({});
  const [otherOpen, setOtherOpen] = useState(false);
  const toggleMore = useCallback((path: string) => setExpandedMore((current) => ({ ...current, [path]: !current[path] })), []);
  const toggleOther = useCallback(() => setOtherOpen((open) => !open), []);

  const rows = useMemo(() => {
    const out: SidebarRow[] = [];
    const explicit = (path: string): boolean | undefined => projectExpanded[path] ?? projectExpanded[normalize(path)];
    const activeThread = activeId ? (threads[activeId] ?? openSummary) : undefined;
    const inActive = new Set(groups.attention.map((thread) => thread.id));
    const calm = (list: ThreadSummary[]) => list.filter((thread) => !inActive.has(thread.id));

    if (groups.attention.length > 0) {
      out.push({ kind: "heading", key: "h:active", heading: "active", count: groups.attention.length });
      for (const thread of groups.attention) out.push({ kind: "thread", key: `a:${thread.id}`, thread, context: basename(thread.cwd) || undefined });
    }

    const quick = calm(groups.quick);
    if (quick.length > 0) {
      out.push({ kind: "heading", key: "h:quick", heading: "quick", count: quick.length });
      const expanded = !!expandedMore[QUICK_KEY];
      const activeIndex = quick.findIndex((thread) => thread.id === activeId);
      const limit = expanded ? quick.length : Math.max(PAGE, activeIndex + 1);
      for (const thread of quick.slice(0, limit)) out.push({ kind: "thread", key: `q:${thread.id}`, thread });
      const hidden = quick.length - Math.min(limit, quick.length);
      if (hidden > 0 || expanded) out.push({ kind: "more", key: "m:quick", path: QUICK_KEY, hidden, remote: 0, expanded, flat: true });
    }

    out.push({ kind: "heading", key: "h:projects", heading: "projects", count: 0 });
    const projectsStart = out.length;

    const add = (entry: ProjectEntry, defaultExpanded: boolean, suffix?: string) => {
      const { project, list } = entry;
      const collapsed = !(explicit(project.path) ?? defaultExpanded);
      const attention = !collapsed
        ? null
        : list.some((thread) => thread.status === "awaiting_input")
          ? "bg-warning"
          : list.some((thread) => thread.status === "error")
            ? "bg-destructive"
            : list.some((thread) => thread.status === "running")
              ? "bg-info"
              : null;
      out.push({ kind: "project", key: `p:${project.path}`, project, suffix, count: list.length, collapsed, attention });
      if (collapsed) return;
      const total = filters.showInternal ? project.threadCount : project.rootThreadCount;
      const remote = filtering || scoped || projectCursors[project.path] === null ? 0 : Math.max(0, total - (groups.loaded.get(entry.key) ?? 0));
      const visible = calm(list);
      if (visible.length === 0) {
        if (remote > 0) out.push({ kind: "more", key: `m:${project.path}`, path: project.path, hidden: 0, remote, expanded: true });
        else if (list.length === 0) out.push({ kind: "empty", key: `e:${project.path}`, text: project.exists ? t("sidebar.noThreads") : t("sidebar.missing") });
        return;
      }
      const expanded = !!expandedMore[project.path];
      const activeIndex = visible.findIndex((thread) => thread.id === activeId || thread.id === activeThread?.parent?.threadId);
      const limit = expanded ? visible.length : Math.max(PAGE, activeIndex + 1);
      for (const thread of visible.slice(0, limit)) out.push({ kind: "thread", key: `t:${thread.id}`, thread });
      const hidden = visible.length - Math.min(limit, visible.length);
      if (hidden > 0 || remote > 0 || expanded) out.push({ kind: "more", key: `m:${project.path}`, path: project.path, hidden, remote, expanded });
    };

    const ruleExpanded = (entry: ProjectEntry) =>
      entry.project.pinned ||
      entry.containsActive ||
      entry.list.some((thread) => thread.status === "running" || thread.status === "awaiting_input");
    const workspaces = groups.workspaces.filter((entry) => !filtering || entry.list.length > 0);
    const fallback = workspaces.some(ruleExpanded) ? null : (workspaces[0]?.project.path ?? null);
    for (const entry of workspaces) add(entry, ruleExpanded(entry) || entry.project.path === fallback, groups.suffixes.get(entry.project.path));

    const older = groups.older.filter((entry) => !filtering || entry.list.length > 0);
    const otherEntry: ProjectEntry | null =
      groups.other.length > 0
        ? {
            project: {
              path: "",
              name: t("sidebar.other"),
              exists: true,
              pinned: false,
              hidden: false,
              threadCount: 0,
              rootThreadCount: 0,
              lastActivityAt: null,
              lastRootActivityAt: null,
            },
            key: "",
            list: groups.other,
            latest: groups.other[0]!.updatedAt,
            containsActive: false,
          }
        : null;
    const count = older.length + (otherEntry ? 1 : 0);
    if (count > 0) {
      out.push({ kind: "section", key: "section:older", count, open: otherOpen });
      if (otherOpen) {
        for (const entry of older) add(entry, false, groups.suffixes.get(entry.project.path));
        if (otherEntry) add(otherEntry, false);
      }
    }
    if (out.length === projectsStart) {
      out.push({ kind: "empty", key: "e:projects", text: filtering || scoped ? t("sidebar.noMatches") : t("sidebar.noProjects") });
    }
    if (threadsCursor) out.push({ kind: "loadall", key: "loadall" });
    return out;
  }, [groups, projectExpanded, expandedMore, otherOpen, activeId, threads, openSummary, filtering, scoped, filters.showInternal, t, projectCursors, threadsCursor]);

  return (
    <RenameContext value={setRenaming}>
      <div className="flex h-full min-h-0 flex-col">
        <div className="flex h-(--workspace-topbar-height) shrink-0 items-center gap-1 ps-4 pe-2">
          <VetroMark className="size-4.5" />
          <span className="ms-1 flex-1 truncate font-semibold text-sm tracking-tight">{t("app.name")}</span>
          <FilterMenu />
          <Tip label={t("sidebar.collapse")}>
            <Button size="icon-xs" variant="ghost-muted" aria-label={t("sidebar.collapse")} className="max-md:hidden" onClick={actions.toggleSidebar}>
              <PanelLeftCloseIcon />
            </Button>
          </Tip>
        </div>
        <div className="flex shrink-0 flex-col gap-px px-2 pb-2">
          <SidebarAction
            icon={SquarePenIcon}
            label={t("sidebar.newThread")}
            active={route.view === "new"}
            onClick={() => actions.navigate({ view: "new", projectPath: null, agentId: null })}
          />
          <SidebarAction
            icon={SearchIcon}
            label={t("sidebar.search")}
            onClick={() => useStore.setState({ paletteOpen: true })}
            trailing={<Kbd className="pointer-coarse:hidden">{isMac ? "⌘K" : "Ctrl K"}</Kbd>}
          />
        </div>
        <VirtualRows rows={rows} activeId={activeId} now={now} onToggleMore={toggleMore} onToggleSection={toggleOther} />
        <div className="shrink-0 px-2 pt-1 pb-[calc(0.5rem+env(safe-area-inset-bottom))]">
          <SidebarAction
            icon={SettingsIcon}
            label={t("sidebar.settings")}
            active={route.view === "settings"}
            onClick={() => actions.navigate({ view: "settings", section: "agents" })}
            trailing={
              connection !== "open" ? (
                <span className="size-1.5 rounded-full bg-warning animate-status-pulse" aria-label={t("conn.lost")} />
              ) : null
            }
          />
        </div>
      </div>
      {renaming ? (
        <RenameDialog open onOpenChange={(open) => !open && setRenaming(null)} threadId={renaming.id} title={renaming.title} />
      ) : null}
    </RenameContext>
  );
}

interface ProjectEntry {
  project: Project;
  key: string;
  list: ThreadSummary[];
  latest: number;
  containsActive: boolean;
}
