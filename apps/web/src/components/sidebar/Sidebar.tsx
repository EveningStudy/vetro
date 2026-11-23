import {
  ArchiveIcon,
  ChevronDownIcon,
  CircleAlertIcon,
  FolderIcon,
  FolderXIcon,
  ListFilterIcon,
  LoaderCircleIcon,
  MessageCircleQuestionIcon,
  MoreHorizontalIcon,
  PinIcon,
  PinOffIcon,
  SearchIcon,
  SettingsIcon,
  SquarePenIcon,
  CornerDownRightIcon,
  EyeIcon,
  EyeOffIcon,
} from "lucide-react";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Project, ThreadSource, ThreadSummary } from "~/api/types";
import { useT } from "~/i18n";
import { relativeTime } from "~/lib/format";
import { cn, isMac } from "~/lib/utils";
import { actions, useStore } from "~/state/store";
import { AgentIcon } from "../AgentIcon";
import { useNow } from "../hooks";
import { Button } from "../ui/button";
import {
  Menu,
  MenuCheckboxItem,
  MenuGroupLabel,
  MenuItem,
  MenuPopup,
  MenuSeparator,
  MenuTrigger,
  Tip,
} from "../ui/overlay";
import { Kbd } from "../ui/primitives";
import { VetroMark } from "../VetroMark";

const SOURCES: ThreadSource[] = ["vetro", "desktop", "cli", "exec", "ide", "cloud", "unknown"];
const PAGE = 8;

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

function StatusLabel({ thread }: { thread: ThreadSummary }) {
  const t = useT();
  if (thread.status === "running")
    return (
      <span className="inline-flex items-center gap-1 text-info-foreground text-xs font-medium">
        <LoaderCircleIcon className="size-3 animate-spin" />
        {t("status.running")}
      </span>
    );
  if (thread.status === "awaiting_input")
    return (
      <span className="inline-flex items-center gap-1 text-warning-foreground text-xs font-medium">
        <MessageCircleQuestionIcon className="size-3" />
        {t("status.awaiting_input")}
      </span>
    );
  if (thread.status === "error")
    return (
      <span className="inline-flex items-center gap-1 text-error-foreground text-xs font-medium">
        <CircleAlertIcon className="size-3" />
        {t("status.error")}
      </span>
    );
  return null;
}

const ROW_HEIGHT = { project: 40, thread: 37, nested: 33, more: 29, empty: 29 } as const;

type SidebarRow =
  | { kind: "project"; key: string; project: Pick<Project, "path" | "name" | "exists" | "pinned" | "hidden">; suffix?: string; count: number; collapsed: boolean; attention: string | null }
  | { kind: "thread"; key: string; thread: ThreadSummary; nested: boolean }
  | { kind: "more"; key: string; path: string; hidden: number; expanded: boolean }
  | { kind: "empty"; key: string; text: string };

const ThreadRow = memo(function ThreadRow({
  thread,
  active,
  nested = false,
  now,
}: {
  thread: ThreadSummary;
  active: boolean;
  nested?: boolean;
  now: number;
}) {
  const t = useT();
  const busy = thread.status !== "idle";
  return (
    <button
      type="button"
      aria-current={active ? "page" : undefined}
      onClick={() => actions.navigate({ view: "thread", threadId: thread.id })}
      className={cn(
        rowBase,
        "pe-2.5",
        nested ? "h-8 ps-9" : "h-9 ps-3",
        active
          ? "bg-sidebar-row-active text-sidebar-foreground light:ring-1 light:ring-inset light:ring-sidebar-border/70"
          : "text-sidebar-foreground hover:bg-sidebar-row-hover",
      )}
    >
      {nested ? <CornerDownRightIcon className="-ms-4 size-3 shrink-0 text-sidebar-muted-foreground/70" /> : null}
      <span
        className={cn(
          "flex shrink-0 items-center transition-[opacity,filter]",
          !active && !busy && "opacity-60 grayscale group-hover/sidebar-row:opacity-100 group-hover/sidebar-row:grayscale-0",
        )}
      >
        <AgentIcon agentId={thread.agentId} className={nested ? "size-3.5" : "size-4"} />
      </span>
      <span
        className={cn(
          "min-w-0 flex-1 truncate text-start",
          nested ? "text-[13px]" : "text-sm",
          active || busy ? "font-medium text-sidebar-foreground" : "text-sidebar-foreground/80 group-hover/sidebar-row:text-sidebar-foreground",
          thread.archived && "italic",
        )}
      >
        {thread.title}
      </span>
      {thread.archived ? <ArchiveIcon className="size-3 shrink-0 text-sidebar-muted-foreground" /> : null}
      <span className="ml-auto flex h-6 shrink-0 items-center justify-end tabular-nums text-secondary-label text-xs">
        {busy ? <StatusLabel thread={thread} /> : relativeTime(thread.updatedAt, t, now)}
      </span>
    </button>
  );
});

const ProjectHeader = memo(function ProjectHeader({ row }: { row: Extract<SidebarRow, { kind: "project" }> }) {
  const t = useT();
  const { project, collapsed, count, attention, suffix } = row;
  return (
    <div className="group/project-folder relative pt-2">
      <button
        type="button"
        aria-expanded={!collapsed}
        onClick={() => actions.toggleProject(project.path)}
        title={project.path || undefined}
        className="flex h-8 w-full cursor-pointer items-center gap-2.5 rounded-(--control-radius) px-(--sidebar-row-content-inset) text-left outline-none hover:bg-sidebar-row-hover focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
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
        {project.hidden ? <EyeOffIcon className="size-3 shrink-0 text-sidebar-muted-foreground" /> : null}
        {attention ? <span className={cn("size-1.5 shrink-0 rounded-full", attention)} /> : null}
        {collapsed && count > 0 ? (
          <span className="shrink-0 text-xs tabular-nums text-sidebar-muted-foreground group-hover/project-folder:invisible pointer-coarse:invisible">{count}</span>
        ) : null}
        <ChevronDownIcon className={cn("size-3 shrink-0 text-sidebar-muted-foreground transition-transform", collapsed && "-rotate-90")} />
      </button>
      {project.path === "" ? null : (
        <span className="absolute top-3 right-6 flex items-center gap-0.5 opacity-0 focus-within:opacity-100 group-hover/project-folder:opacity-100 pointer-coarse:opacity-100">
          <Menu>
            <MenuTrigger render={<Button size="icon-xs" variant="ghost-muted" aria-label={t("thread.more")} />}>
              <MoreHorizontalIcon />
            </MenuTrigger>
            <MenuPopup align="end">
              <MenuItem onClick={() => void actions.updateProject(project.path, { pinned: !project.pinned })}>
                {project.pinned ? <PinOffIcon /> : <PinIcon />}
                {project.pinned ? t("sidebar.unpin") : t("sidebar.pin")}
              </MenuItem>
              <MenuItem onClick={() => void actions.updateProject(project.path, { hidden: !project.hidden })}>
                {project.hidden ? <EyeIcon /> : <EyeOffIcon />}
                {project.hidden ? t("sidebar.unhide") : t("sidebar.hide")}
              </MenuItem>
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
      )}
    </div>
  );
});

/** The sidebar can hold hundreds of projects and threads; only the rows near the viewport are
 * rendered. Every row kind has a fixed height, so offsets are exact. */
function VirtualRows({
  rows,
  activeId,
  now,
  onToggleMore,
}: {
  rows: SidebarRow[];
  activeId: string | null;
  now: number;
  onToggleMore: (path: string) => void;
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
                <ThreadRow thread={row.thread} nested={row.nested} active={row.thread.id === activeId} now={now} />
              ) : row.kind === "more" ? (
                <MoreRow row={row} onToggle={onToggleMore} />
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

function MoreRow({ row, onToggle }: { row: Extract<SidebarRow, { kind: "more" }>; onToggle: (path: string) => void }) {
  const t = useT();
  return (
    <button
      type="button"
      onClick={() => onToggle(row.path)}
      className="h-7 w-full rounded-md ps-9 text-left text-xs text-sidebar-muted-foreground hover:bg-sidebar-row-hover hover:text-sidebar-foreground"
    >
      {row.expanded ? t("sidebar.showLess") : t("sidebar.showMore", { n: row.hidden })}
    </button>
  );
}

function rowHeight(row: SidebarRow) {
  return row.kind === "thread" ? (row.nested ? ROW_HEIGHT.nested : ROW_HEIGHT.thread) : ROW_HEIGHT[row.kind];
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
  const active = filters.agentIds.length + filters.sources.length + (filters.showArchived ? 1 : 0) + (filters.showHidden ? 1 : 0);
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
        <MenuGroupLabel>{t("sidebar.filterSources")}</MenuGroupLabel>
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
        <MenuSeparator />
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
        {active > 0 ? (
          <MenuItem onClick={() => actions.setFilters({ agentIds: [], sources: [], showArchived: false, showHidden: false })}>
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
  const route = useStore((s) => s.route);
  const connection = useStore((s) => s.connection);
  const activeId = route.view === "thread" ? route.threadId : null;

  const groups = useMemo(() => {
    const all = Object.values(threads).sort((a, b) => b.updatedAt - a.updatedAt);
    const matches = (thread: ThreadSummary) =>
      (filters.showArchived || !thread.archived) &&
      (filters.agentIds.length === 0 || filters.agentIds.includes(thread.agentId)) &&
      (filters.sources.length === 0 || filters.sources.includes(thread.source));
    const children = new Map<string, ThreadSummary[]>();
    for (const thread of all) {
      if (!thread.parent || !matches(thread)) continue;
      const list = children.get(thread.parent.threadId) ?? [];
      list.push(thread);
      children.set(thread.parent.threadId, list);
    }
    const byProject = new Map<string, ThreadSummary[]>();
    for (const thread of all) {
      if (thread.parent && threads[thread.parent.threadId]) continue;
      if (!matches(thread)) continue;
      const key = normalize(thread.cwd);
      byProject.set(key, [...(byProject.get(key) ?? []), thread]);
    }
    const visibleProjects = projects
      .filter((project) => filters.showHidden || !project.hidden)
      .sort((a, b) => Number(b.pinned) - Number(a.pinned) || (b.lastActivityAt ?? 0) - (a.lastActivityAt ?? 0));
    const known = new Set(projects.map((project) => normalize(project.path)));
    const other = [...byProject.entries()].filter(([key]) => !known.has(key)).flatMap(([, list]) => list);
    const nameCounts = new Map<string, number>();
    for (const project of visibleProjects) nameCounts.set(project.name.toLowerCase(), (nameCounts.get(project.name.toLowerCase()) ?? 0) + 1);
    const suffixes = new Map<string, string>();
    for (const project of visibleProjects) {
      if ((nameCounts.get(project.name.toLowerCase()) ?? 0) < 2) continue;
      const parts = project.path.split(/[\\/]/).filter(Boolean);
      suffixes.set(project.path, parts.at(-2) ?? "");
    }
    return { visibleProjects, byProject, children, other, suffixes };
  }, [threads, projects, filters]);

  const filtering = filters.agentIds.length > 0 || filters.sources.length > 0;
  const collapsedProjects = useStore((s) => s.collapsedProjects);
  const [expandedMore, setExpandedMore] = useState<Record<string, boolean>>({});
  const toggleMore = useCallback((path: string) => setExpandedMore((current) => ({ ...current, [path]: !current[path] })), []);

  const rows = useMemo(() => {
    const out: SidebarRow[] = [];
    const add = (project: Extract<SidebarRow, { kind: "project" }>["project"], list: ThreadSummary[], suffix?: string) => {
      if (filtering && list.length === 0) return;
      const collapsed = !!collapsedProjects[project.path];
      const attention = !collapsed
        ? null
        : list.some((thread) => thread.status === "awaiting_input")
          ? "bg-warning"
          : list.some((thread) => thread.status === "running")
            ? "bg-info"
            : null;
      out.push({ kind: "project", key: `p:${project.path}`, project, suffix, count: list.length, collapsed, attention });
      if (collapsed) return;
      if (list.length === 0) {
        out.push({ kind: "empty", key: `e:${project.path}`, text: project.exists ? t("sidebar.noThreads") : t("sidebar.missing") });
        return;
      }
      const expanded = !!expandedMore[project.path];
      const activeIndex = list.findIndex(
        (thread) => thread.id === activeId || groups.children.get(thread.id)?.some((child) => child.id === activeId),
      );
      const limit = expanded ? list.length : Math.max(PAGE, activeIndex + 1);
      for (const thread of list.slice(0, limit)) {
        out.push({ kind: "thread", key: `t:${thread.id}`, thread, nested: false });
        for (const child of groups.children.get(thread.id) ?? []) out.push({ kind: "thread", key: `c:${child.id}`, thread: child, nested: true });
      }
      const hidden = list.length - Math.min(limit, list.length);
      if (hidden > 0 || expanded) out.push({ kind: "more", key: `m:${project.path}`, path: project.path, hidden, expanded });
    };
    for (const project of groups.visibleProjects) add(project, groups.byProject.get(normalize(project.path)) ?? [], groups.suffixes.get(project.path));
    if (groups.other.length > 0) add({ path: "", name: t("sidebar.other"), exists: true, pinned: false, hidden: false }, groups.other);
    return out;
  }, [groups, collapsedProjects, expandedMore, activeId, filtering, t]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 ps-4 pe-2">
        <VetroMark className="size-4.5" />
        <span className="flex-1 truncate font-semibold text-sm tracking-tight">{t("app.name")}</span>
        <FilterMenu />
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
          trailing={
            <Kbd className="pointer-coarse:hidden">{isMac ? "⌘K" : "Ctrl K"}</Kbd>
          }
        />
      </div>
      <div className="shrink-0 px-(--sidebar-row-content-inset) pt-1 pb-0.5 ps-[calc(var(--sidebar-row-content-inset)+0.5rem)] text-xs font-medium text-sidebar-muted-foreground">
        {t("sidebar.projects")}
      </div>
      {rows.length === 0 ? (
        <div className="min-h-0 flex-1 px-4 py-2 text-xs text-sidebar-muted-foreground">
          {filtering ? t("sidebar.noMatches") : t("sidebar.noProjects")}
        </div>
      ) : (
        <VirtualRows rows={rows} activeId={activeId} now={now} onToggleMore={toggleMore} />
      )}
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
  );
}
