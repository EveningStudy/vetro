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
  EyeOffIcon,
} from "lucide-react";
import { memo, useMemo, useState } from "react";
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
  const status = <StatusLabel thread={thread} />;
  const busy = thread.status !== "idle";
  return (
    <li className="list-none [content-visibility:auto] [contain-intrinsic-size:auto_--spacing(9)]">
      <button
        type="button"
        aria-current={active ? "page" : undefined}
        onClick={() => actions.navigate({ view: "thread", threadId: thread.id })}
        className={cn(
          rowBase,
          "h-9 pe-2.5",
          nested ? "h-8 ps-9" : "ps-3",
          active
            ? "bg-sidebar-row-active text-sidebar-foreground light:ring-1 light:ring-inset light:ring-sidebar-border/70"
            : "text-sidebar-foreground hover:bg-sidebar-row-hover",
        )}
      >
        {nested ? (
          <CornerDownRightIcon className="-ms-4 size-3 shrink-0 text-sidebar-muted-foreground/70" />
        ) : null}
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
          {busy ? status : relativeTime(thread.updatedAt, t, now)}
        </span>
      </button>
    </li>
  );
});

function ProjectGroup({
  project,
  threads,
  childMap,
  activeId,
  now,
}: {
  project: Pick<Project, "path" | "name" | "exists" | "pinned">;
  threads: ThreadSummary[];
  childMap: Map<string, ThreadSummary[]>;
  activeId: string | null;
  now: number;
}) {
  const t = useT();
  const collapsed = useStore((s) => !!s.collapsedProjects[project.path]);
  const [expanded, setExpanded] = useState(false);
  const activeIndex = threads.findIndex((thread) => thread.id === activeId || childMap.get(thread.id)?.some((c) => c.id === activeId));
  const limit = expanded ? threads.length : Math.max(PAGE, activeIndex + 1);
  const visible = threads.slice(0, limit);
  const hidden = threads.length - visible.length;
  const attention = collapsed
    ? threads.some((thread) => thread.status === "awaiting_input")
      ? "bg-warning"
      : threads.some((thread) => thread.status === "running")
        ? "bg-info"
        : null
    : null;
  const isOther = project.path === "";
  return (
    <li className="group/project-folder relative mt-2 list-none first:mt-0">
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
        <span className={cn("min-w-0 flex-1 truncate text-sm font-medium text-sidebar-foreground/90", !project.exists && "text-sidebar-muted-foreground")}>
          {project.name}
        </span>
        {attention ? <span className={cn("size-1.5 shrink-0 rounded-full", attention)} /> : null}
        {collapsed && threads.length > 0 ? (
          <span className="shrink-0 text-xs tabular-nums text-sidebar-muted-foreground group-hover/project-folder:invisible pointer-coarse:invisible">
            {threads.length}
          </span>
        ) : null}
        <ChevronDownIcon className={cn("size-3 shrink-0 text-sidebar-muted-foreground transition-transform", collapsed && "-rotate-90")} />
      </button>
      {isOther ? null : (
        <span className="absolute top-1 right-6 flex items-center gap-0.5 opacity-0 focus-within:opacity-100 group-hover/project-folder:opacity-100 pointer-coarse:opacity-100">
          <Menu>
            <MenuTrigger render={<Button size="icon-xs" variant="ghost-muted" aria-label={t("thread.more")} />}>
              <MoreHorizontalIcon />
            </MenuTrigger>
            <MenuPopup align="end">
              <MenuItem onClick={() => void actions.updateProject(project.path, { pinned: !project.pinned })}>
                {project.pinned ? <PinOffIcon /> : <PinIcon />}
                {project.pinned ? t("sidebar.unpin") : t("sidebar.pin")}
              </MenuItem>
              <MenuItem onClick={() => void actions.updateProject(project.path, { hidden: true })}>
                <EyeOffIcon />
                {t("sidebar.hide")}
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
      {collapsed ? null : (
        <ul className="mt-0.5 flex flex-col gap-px">
          {visible.length === 0 ? (
            <li className="list-none px-3 py-1.5 ps-9 text-xs text-sidebar-muted-foreground">
              {project.exists ? t("sidebar.noThreads") : t("sidebar.missing")}
            </li>
          ) : null}
          {visible.map((thread) => (
            <ThreadWithChildren key={thread.id} thread={thread} childThreads={childMap.get(thread.id)} activeId={activeId} now={now} />
          ))}
          {hidden > 0 || expanded ? (
            <li className="list-none">
              <button
                type="button"
                onClick={() => setExpanded(!expanded)}
                className="h-7 w-full rounded-md ps-9 text-left text-xs text-sidebar-muted-foreground hover:bg-sidebar-row-hover hover:text-sidebar-foreground"
              >
                {expanded ? t("sidebar.showLess") : t("sidebar.showMore", { n: hidden })}
              </button>
            </li>
          ) : null}
        </ul>
      )}
    </li>
  );
}

function ThreadWithChildren({
  thread,
  childThreads,
  activeId,
  now,
}: {
  thread: ThreadSummary;
  childThreads: ThreadSummary[] | undefined;
  activeId: string | null;
  now: number;
}) {
  return (
    <>
      <ThreadRow thread={thread} active={thread.id === activeId} now={now} />
      {childThreads?.map((child) => (
        <ThreadRow key={child.id} thread={child} active={child.id === activeId} nested now={now} />
      ))}
    </>
  );
}

function FilterMenu() {
  const t = useT();
  const filters = useStore((s) => s.filters);
  const agents = useStore((s) => s.agents);
  const active = filters.agentIds.length + filters.sources.length + (filters.showArchived ? 1 : 0);
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
        {active > 0 ? (
          <MenuItem onClick={() => actions.setFilters({ agentIds: [], sources: [], showArchived: false })}>
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
      .filter((project) => !project.hidden)
      .sort((a, b) => Number(b.pinned) - Number(a.pinned) || (b.lastActivityAt ?? 0) - (a.lastActivityAt ?? 0));
    const known = new Set(projects.map((project) => normalize(project.path)));
    const other = [...byProject.entries()].filter(([key]) => !known.has(key)).flatMap(([, list]) => list);
    return { visibleProjects, byProject, children, other };
  }, [threads, projects, filters]);

  const filtering = filters.agentIds.length > 0 || filters.sources.length > 0;

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
      <nav className="min-h-0 flex-1 overflow-y-auto px-2 pb-2 [scrollbar-gutter:stable]">
        <div className="px-(--sidebar-row-content-inset) pt-1 pb-1.5 text-xs font-medium text-sidebar-muted-foreground">
          {t("sidebar.projects")}
        </div>
        <ul>
          {groups.visibleProjects.length === 0 && groups.other.length === 0 ? (
            <li className="list-none px-3 py-2 text-xs text-sidebar-muted-foreground">
              {filtering ? t("sidebar.noMatches") : t("sidebar.noProjects")}
            </li>
          ) : null}
          {groups.visibleProjects.map((project) => {
            const list = groups.byProject.get(normalize(project.path)) ?? [];
            if (filtering && list.length === 0) return null;
            return (
              <ProjectGroup
                key={project.path}
                project={project}
                threads={list}
                childMap={groups.children}
                activeId={activeId}
                now={now}
              />
            );
          })}
          {groups.other.length > 0 ? (
            <ProjectGroup
              project={{ path: "", name: t("sidebar.other"), exists: true, pinned: false }}
              threads={groups.other}
              childMap={groups.children}
              activeId={activeId}
              now={now}
            />
          ) : null}
        </ul>
      </nav>
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
