import { create } from "zustand";
import { authApi, client } from "~/api/client";
import type { ConnectionStatus } from "~/api/protocol";
import type {
  AgentInfo,
  AppInfo,
  ConfigOption,
  ContentBlock,
  GitStatusResult,
  Item,
  OpenTarget,
  Project,
  RemoteStatus,
  RequestResolution,
  ServerEvent,
  Settings,
  SlashCommand,
  ThreadEvent,
  ThreadSource,
  ThreadSummary,
  ThreadUsage,
} from "~/api/types";
import { translate } from "~/i18n";
import { connectPrefs, getPrefs, onPrefsLoaded, type PrefsData, resolvedLanguage, savePrefs, setSidebarCollapsed, usePrefs } from "./prefs";
import { errorMessage, toast } from "./toast";

export interface OpenThread {
  summary: ThreadSummary;
  items: Item[];
  hasMore: boolean;
  seq: number;
  options: ConfigOption[];
  commands: SlashCommand[];
  usage: ThreadUsage;
  loadingOlder: boolean;
}

export type Route =
  | { view: "empty" }
  | { view: "thread"; threadId: string }
  | { view: "new"; projectPath: string | null; agentId: string | null }
  | { view: "settings"; section: SettingsSection };

export type SettingsSection = "agents" | "remote" | "appearance" | "about";

export type RightDrawer =
  | { kind: "raw" }
  | { kind: "changes" }
  | { kind: "children"; parentId: string }
  | { kind: "child"; threadId: string; fromParentId?: string };

export interface Filters {
  agentIds: string[];
  sources: ThreadSource[];
  showArchived: boolean;
  showHidden: boolean;
  showInternal: boolean;
}

interface State {
  connection: ConnectionStatus;
  app: AppInfo | null;
  agents: AgentInfo[];
  projects: Project[];
  settings: Settings | null;
  threads: Record<string, ThreadSummary>;
  threadsLoaded: boolean;
  open: Record<string, OpenThread>;
  remote: RemoteStatus | null;
  route: Route;
  filters: Filters;
  projectExpanded: Record<string, boolean>;
  rightDrawer: RightDrawer | null;
  paletteOpen: boolean;
  mobileSidebarOpen: boolean;
  drafts: Record<string, string>;
  authLost: boolean;
  /** Cursor of the newest-first list; null when everything is loaded. */
  threadsCursor: string | null;
  /** Per project: undefined = not paged yet, null = all of it loaded, string = more after this. */
  projectCursors: Record<string, string | null>;
  loadingMore: boolean;
  /** Threads that finished while the user was looking elsewhere; cleared when the thread is opened. */
  unreadCompleted: Record<string, boolean>;
  /** Read-only Git summary per working directory, fetched when a thread opens or a turn ends. */
  gitStatus: Record<string, GitStatusResult>;
}

export function routeFromLocation(): Route {
  const path = location.pathname.replace(/\/+$/, "");
  const query = new URLSearchParams(location.search);
  const thread = /^\/t\/(.+)$/.exec(path);
  if (thread) return { view: "thread", threadId: decodeURIComponent(thread[1]!) };
  if (path === "/new") return { view: "new", projectPath: query.get("project"), agentId: query.get("agent") };
  const settings = /^\/settings(?:\/(\w+))?$/.exec(path);
  if (settings) {
    const section = (settings[1] ?? "agents") as SettingsSection;
    return { view: "settings", section: ["agents", "remote", "appearance", "about"].includes(section) ? section : "agents" };
  }
  return { view: "empty" };
}

function routeToUrl(route: Route) {
  const keep = new URLSearchParams(location.search);
  for (const key of ["project", "agent"]) keep.delete(key);
  let path = "/";
  if (route.view === "thread") path = `/t/${encodeURIComponent(route.threadId)}`;
  if (route.view === "settings") path = `/settings/${route.section}`;
  if (route.view === "new") {
    path = "/new";
    if (route.projectPath) keep.set("project", route.projectPath);
    if (route.agentId) keep.set("agent", route.agentId);
  }
  const search = keep.toString();
  return `${path}${search ? `?${search}` : ""}`;
}

function initialDrawer(panel: string | null): RightDrawer | null {
  if (panel === "raw") return { kind: "raw" };
  if (panel === "changes") return { kind: "changes" };
  return null;
}

export const useStore = create<State>(() => ({
  connection: "connecting",
  app: null,
  agents: [],
  projects: [],
  settings: null,
  threads: {},
  threadsLoaded: false,
  open: {},
  remote: null,
  route: routeFromLocation(),
  filters: { agentIds: [], sources: [], showArchived: false, showHidden: false, showInternal: false, ...(getPrefs().filters as Partial<Filters> | undefined) },
  projectExpanded: getPrefs().projectExpanded ?? {},
  rightDrawer: initialDrawer(new URLSearchParams(location.search).get("panel")),
  paletteOpen: new URLSearchParams(location.search).get("palette") === "1",
  mobileSidebarOpen: new URLSearchParams(location.search).get("sidebar") === "1",
  drafts: {},
  authLost: false,
  threadsCursor: null,
  projectCursors: {},
  loadingMore: false,
  unreadCompleted: {},
  gitStatus: {},
}));

const set = useStore.setState;
const get = useStore.getState;

function sortItems(items: Item[]) {
  return items.sort((a, b) => a.ordinal - b.ordinal);
}

function upsertItem(items: Item[], item: Item) {
  const index = items.findIndex((existing) => existing.id === item.id);
  if (index < 0) return sortItems([...items, item]);
  const next = items.slice();
  next[index] = item;
  return next;
}

function patchOpen(threadId: string, patch: (thread: OpenThread) => Partial<OpenThread>) {
  const thread = get().open[threadId];
  if (!thread) return;
  set({ open: { ...get().open, [threadId]: { ...thread, ...patch(thread) } } });
}

const BUSY = new Set<ThreadSummary["status"]>(["running", "awaiting_input"]);

function isThreadVisible(threadId: string) {
  const route = get().route;
  return route.view === "thread" && route.threadId === threadId && !document.hidden;
}

function notify(thread: ThreadSummary, key: "notify.finished" | "notify.needsInput") {
  const { app, settings } = get();
  if (!app || app.remoteClient || settings?.notificationsEnabled === false) return;
  const lang = resolvedLanguage(usePrefs.getState().language);
  void client()
    .request("app.notify", { title: thread.title, body: translate(lang, key), threadId: thread.id })
    .catch(() => undefined);
}

/** Status transitions of threads the user is not looking at: mark finished ones unread and ask the
 * desktop for a notification. */
function noticeTransitions(threads: ThreadSummary[]) {
  let unread: Record<string, boolean> | null = null;
  for (const thread of threads) {
    const before = get().threads[thread.id] ?? get().open[thread.id]?.summary;
    if (!before || before.status === thread.status || thread.parent || isThreadVisible(thread.id)) continue;
    if (thread.status === "idle" && BUSY.has(before.status)) {
      unread ??= { ...get().unreadCompleted };
      unread[thread.id] = true;
      notify(thread, "notify.finished");
    } else if (thread.status === "awaiting_input") {
      notify(thread, "notify.needsInput");
    }
  }
  if (unread) set({ unreadCompleted: unread });
}

function clearUnread(threadId: string) {
  if (!get().unreadCompleted[threadId]) return;
  const unreadCompleted = { ...get().unreadCompleted };
  delete unreadCompleted[threadId];
  set({ unreadCompleted });
}

function mergeSummaries(threads: ThreadSummary[]) {
  noticeTransitions(threads);
  const next = { ...get().threads };
  for (const thread of threads) next[thread.id] = thread;
  const open = { ...get().open };
  for (const thread of threads) if (open[thread.id]) open[thread.id] = { ...open[thread.id]!, summary: thread };
  set({ threads: next, open });
}

function applyThreadEvent(threadId: string, event: ThreadEvent) {
  switch (event.type) {
    case "item_added":
    case "item_updated":
      patchOpen(threadId, (thread) => ({ items: upsertItem(thread.items, event.item) }));
      break;
    case "text_delta":
      patchOpen(threadId, (thread) => ({
        items: thread.items.map((item) =>
          item.id === event.itemId && (item.type === "agent_message" || item.type === "reasoning")
            ? { ...item, text: item.text + event.delta }
            : item,
        ),
      }));
      break;
    case "summary":
      mergeSummaries([event.thread]);
      break;
    case "options":
      patchOpen(threadId, () => ({ options: event.options }));
      break;
    case "commands":
      patchOpen(threadId, () => ({ commands: event.commands }));
      break;
    case "usage":
      patchOpen(threadId, () => ({ usage: event.usage }));
      break;
  }
}

function handleEvent(event: ServerEvent) {
  switch (event.event) {
    case "thread.event": {
      const { threadId, seq } = event.data;
      const thread = get().open[threadId];
      if (!thread || seq <= thread.seq) return;
      if (seq !== thread.seq + 1) {
        void actions.openThread(threadId, true);
        return;
      }
      patchOpen(threadId, () => ({ seq }));
      applyThreadEvent(threadId, event.data.event);
      break;
    }
    case "threads.upserted":
      mergeSummaries(event.data.threads);
      break;
    case "threads.removed": {
      const threads = { ...get().threads };
      for (const id of event.data.threadIds) delete threads[id];
      set({ threads });
      break;
    }
    case "agents.updated":
      set({ agents: event.data.agents });
      break;
    case "projects.updated":
      set({ projects: event.data.projects });
      break;
    case "remote.updated":
      set({ remote: event.data.status });
      break;
    case "app.navigate":
      actions.navigate({ view: "thread", threadId: event.data.threadId });
      break;
    case "index.progress":
      break;
  }
}

const FIRST_PAGE = 500;
const PROJECT_PAGE = 200;

async function loadLists() {
  const api = client();
  const [agents, projects, settings] = await Promise.all([
    api.request("agents.list", {}),
    api.request("projects.list", {}),
    api.request("settings.get", {}).catch(() => null),
  ]);
  set({ agents: agents.agents, projects: projects.projects, ...(settings ? { settings } : {}) });
  const page = await api.request("threads.list", { ...rootListParams(), limit: FIRST_PAGE });
  const map: Record<string, ThreadSummary> = {};
  const open = get().open;
  for (const thread of Object.values(get().threads)) if (thread.parent && open[thread.parent.threadId]) map[thread.id] = thread;
  for (const thread of page.threads) map[thread.id] = thread;
  set({ threads: map, threadsLoaded: true, threadsCursor: page.nextCursor, projectCursors: {} });
}

/** Sidebar lists hold root threads only, so child sessions never eat into a page. */
function rootListParams() {
  return { includeArchived: true, includeChildren: false, includeInternal: get().filters.showInternal };
}

async function loadChildren(threadId: string) {
  const page = await client().request("threads.list", {
    parentId: threadId,
    includeChildren: true,
    includeInternal: true,
    includeArchived: true,
    limit: 100,
  });
  mergeSummaries(page.threads);
  return page.threads;
}

async function loadPrefs() {
  const api = client();
  const remote = await api
    .request("prefs.get", {})
    .then((result) => (result.prefs && typeof result.prefs === "object" ? (result.prefs as PrefsData) : null))
    .catch(() => undefined);
  if (remote === undefined) return;
  connectPrefs(remote, (prefs) => void api.request("prefs.set", { prefs: prefs as never }).catch(() => undefined));
}

onPrefsLoaded((prefs) => {
  const before = get().filters.showInternal;
  useStore.setState((state) => ({
    projectExpanded: prefs.projectExpanded ?? state.projectExpanded,
    filters: prefs.filters ? { ...state.filters, ...(prefs.filters as Partial<Filters>) } : state.filters,
  }));
  if (get().threadsLoaded && get().filters.showInternal !== before) void loadLists().catch((error) => toast.error(errorMessage(error)));
});

let started = false;

export function startSync() {
  if (started) return;
  started = true;
  const api = client();
  api.onEvent(handleEvent);
  api.onStatus((status, { info, reconnected }) => {
    set({ connection: status, ...(info ? { app: info } : {}) });
    if (status === "closed") {
      authApi()
        .session()
        .then((session) => session === null && set({ authLost: true }))
        .catch(() => undefined);
    }
    if (status !== "open") return;
    if (!reconnected) void loadPrefs();
    void loadLists().catch((error) => toast.error(errorMessage(error)));
    if (reconnected) for (const id of Object.keys(get().open)) void actions.openThread(id, true);
  });
  api.start();
  window.addEventListener("popstate", () => {
    const route = routeFromLocation();
    set({ route, mobileSidebarOpen: false, ...(leavesChildDrawer(get().route, route) ? { rightDrawer: null } : {}) });
    if (route.view === "thread") clearUnread(route.threadId);
  });
  const seen = () => {
    const route = get().route;
    if (route.view === "thread" && !document.hidden) clearUnread(route.threadId);
  };
  document.addEventListener("visibilitychange", seen);
  window.addEventListener("focus", seen);
}

function leavesChildDrawer(from: Route, to: Route) {
  const kind = get().rightDrawer?.kind;
  if (kind !== "child" && kind !== "children") return false;
  return from.view !== to.view || (from.view === "thread" && to.view === "thread" && from.threadId !== to.threadId);
}

const gitInflight = new Map<string, Promise<GitStatusResult | null>>();

const opening = new Map<string, Promise<void>>();
const refs = new Map<string, number>();
/** Each threads.open gets a generation; a late answer to an older request is dropped. */
const generations = new Map<string, number>();
/** Views left while the thread was still loading; close its subscription once the open lands. */
const closeAfterOpen = new Set<string>();

export const actions = {
  navigate(route: Route, replace = false) {
    const url = routeToUrl(route);
    if (replace) history.replaceState(null, "", url);
    else if (url !== `${location.pathname}${location.search}`) history.pushState(null, "", url);
    set({ route, mobileSidebarOpen: false, ...(leavesChildDrawer(get().route, route) ? { rightDrawer: null } : {}) });
    if (route.view === "thread") clearUnread(route.threadId);
  },

  openRawDrawer() {
    set({ rightDrawer: { kind: "raw" } });
  },

  openChangesDrawer() {
    set({ rightDrawer: { kind: "changes" } });
  },

  toggleChangesDrawer() {
    set({ rightDrawer: get().rightDrawer?.kind === "changes" ? null : { kind: "changes" } });
  },

  openChildrenDrawer(parentId: string) {
    set({ rightDrawer: { kind: "children", parentId } });
    void loadChildren(parentId).catch(() => undefined);
  },

  openChildDrawer(threadId: string, fromParentId?: string) {
    set({ rightDrawer: fromParentId ? { kind: "child", threadId, fromParentId } : { kind: "child", threadId } });
  },

  /** Every session linked to a parent, including ones no tool call points at. */
  fetchChildren(parentId: string): Promise<ThreadSummary[]> {
    return loadChildren(parentId);
  },

  toggleSidebar() {
    setSidebarCollapsed(!usePrefs.getState().sidebarCollapsed);
  },

  markSeen(threadId: string) {
    clearUnread(threadId);
  },

  closeDrawer() {
    set({ rightDrawer: null });
  },

  toggleRawDrawer() {
    set({ rightDrawer: get().rightDrawer?.kind === "raw" ? null : { kind: "raw" } });
  },

  openThread(threadId: string, force = false): Promise<void> {
    if (!force && get().open[threadId]) return Promise.resolve();
    const inflight = opening.get(threadId);
    if (inflight && !force) return inflight;
    const generation = (generations.get(threadId) ?? 0) + 1;
    generations.set(threadId, generation);
    const promise = client()
      .request("threads.open", { threadId, limit: 200 })
      .then((result) => {
        if (generations.get(threadId) !== generation) return;
        if (closeAfterOpen.has(threadId)) {
          closeAfterOpen.delete(threadId);
          void client().request("threads.close", { threadId }).catch(() => undefined);
          return;
        }
        set({
          open: {
            ...get().open,
            [threadId]: {
              summary: result.thread,
              items: sortItems(result.items),
              hasMore: result.hasMore,
              seq: result.seq,
              options: result.options,
              commands: result.commands,
              usage: result.usage,
              loadingOlder: false,
            },
          },
          threads: { ...get().threads, [threadId]: result.thread },
        });
        if (result.thread.childCount > 0) void loadChildren(threadId).catch(() => undefined);
      })
      .catch((error) => {
        toast.error(errorMessage(error));
        throw error;
      })
      .finally(() => {
        if (opening.get(threadId) === promise) opening.delete(threadId);
      });
    opening.set(threadId, promise);
    return promise;
  },

  /** Keeps a thread open while at least one view shows it. */
  retain(threadId: string) {
    refs.set(threadId, (refs.get(threadId) ?? 0) + 1);
    closeAfterOpen.delete(threadId);
    void actions.openThread(threadId).catch(() => undefined);
  },

  release(threadId: string) {
    const count = (refs.get(threadId) ?? 1) - 1;
    if (count > 0) {
      refs.set(threadId, count);
      return;
    }
    refs.delete(threadId);
    setTimeout(() => {
      if (!refs.has(threadId)) actions.closeThread(threadId);
    }, 0);
  },

  closeThread(threadId: string) {
    if (opening.has(threadId)) {
      closeAfterOpen.add(threadId);
      return;
    }
    if (!get().open[threadId]) return;
    const open = { ...get().open };
    delete open[threadId];
    set({ open });
    void client().request("threads.close", { threadId }).catch(() => undefined);
  },

  async loadOlder(threadId: string) {
    const thread = get().open[threadId];
    if (!thread || !thread.hasMore || thread.loadingOlder) return;
    const before = thread.items[0]?.ordinal ?? Number.MAX_SAFE_INTEGER;
    patchOpen(threadId, () => ({ loadingOlder: true }));
    try {
      const result = await client().request("threads.items", { threadId, before, limit: 200 });
      patchOpen(threadId, (current) => {
        const known = new Set(current.items.map((item) => item.id));
        return {
          items: sortItems([...result.items.filter((item) => !known.has(item.id)), ...current.items]),
          hasMore: result.hasMore,
          loadingOlder: false,
        };
      });
    } catch (error) {
      patchOpen(threadId, () => ({ loadingOlder: false }));
      toast.error(errorMessage(error));
    }
  },

  async send(threadId: string, content: ContentBlock[]) {
    await client().request("threads.send", { threadId, content });
  },

  async start(agentId: string, cwd: string, content: ContentBlock[], options: Record<string, string>) {
    const { thread } = await client().request("threads.start", { agentId, cwd, content, options });
    mergeSummaries([thread]);
    await actions.openThread(thread.id, true);
    actions.navigate({ view: "thread", threadId: thread.id });
    return thread;
  },

  async interrupt(threadId: string) {
    await client().request("threads.interrupt", { threadId }).catch((error) => toast.error(errorMessage(error)));
  },

  async respond(threadId: string, requestId: string, resolution: RequestResolution) {
    await client()
      .request("threads.respond", { threadId, requestId, resolution })
      .catch((error) => toast.error(errorMessage(error)));
  },

  async setOption(threadId: string, optionId: string, value: string) {
    patchOpen(threadId, (thread) => ({
      options: thread.options.map((option) => (option.id === optionId ? { ...option, currentValue: value } : option)),
    }));
    try {
      const { options } = await client().request("threads.setOption", { threadId, optionId, value });
      patchOpen(threadId, () => ({ options }));
    } catch (error) {
      toast.error(errorMessage(error));
      void actions.openThread(threadId, true);
    }
  },

  async updateThread(threadId: string, patch: { title?: string; archived?: boolean; pinned?: boolean }) {
    try {
      const { thread } = await client().request("threads.update", { threadId, ...patch });
      mergeSummaries([thread]);
    } catch (error) {
      toast.error(errorMessage(error));
    }
  },

  refreshGitStatus(cwd: string): Promise<GitStatusResult | null> {
    const inflight = gitInflight.get(cwd);
    if (inflight) return inflight;
    const promise = client()
      .request("workspace.gitStatus", { cwd })
      .then((status) => {
        set({ gitStatus: { ...get().gitStatus, [cwd]: status } });
        return status;
      })
      .catch(() => null)
      .finally(() => gitInflight.delete(cwd));
    gitInflight.set(cwd, promise);
    return promise;
  },

  async openExternal(cwd: string, target: OpenTarget, path?: string, line?: number) {
    await client()
      .request("workspace.openExternal", { cwd, target, ...(path ? { path } : {}), ...(line ? { line } : {}) })
      .catch((error) => toast.error(errorMessage(error)));
  },

  async detach(threadId: string) {
    await client().request("threads.detach", { threadId }).catch((error) => toast.error(errorMessage(error)));
  },

  async addProject(path: string) {
    const project = await client().request("projects.add", { path });
    if (!get().projects.some((p) => p.path === project.path)) set({ projects: [...get().projects, project] });
    return project;
  },

  /** Next page of the flat, newest-first list. */
  async loadMoreThreads() {
    const cursor = get().threadsCursor;
    if (!cursor || get().loadingMore) return;
    set({ loadingMore: true });
    try {
      const page = await client().request("threads.list", { ...rootListParams(), limit: FIRST_PAGE, cursor });
      mergeSummaries(page.threads);
      set({ threadsCursor: page.nextCursor });
    } catch (error) {
      toast.error(errorMessage(error));
    } finally {
      set({ loadingMore: false });
    }
  },

  /** More threads of one project, independent of the flat list. */
  async loadProjectThreads(path: string) {
    const cursors = get().projectCursors;
    if (cursors[path] === null || get().loadingMore) return;
    set({ loadingMore: true });
    try {
      const cursor = cursors[path];
      const page = await client().request("threads.list", {
        projectPath: path,
        ...rootListParams(),
        limit: PROJECT_PAGE,
        ...(cursor ? { cursor } : {}),
      });
      mergeSummaries(page.threads);
      set({ projectCursors: { ...get().projectCursors, [path]: page.nextCursor } });
    } catch (error) {
      toast.error(errorMessage(error));
    } finally {
      set({ loadingMore: false });
    }
  },

  /** Resolves to false when the folder stays listed because sessions still point at it. */
  async removeProject(path: string) {
    await client().request("projects.remove", { path });
    const { projects } = await client().request("projects.list", {});
    set({ projects });
    return !projects.some((project) => project.path === path);
  },

  async updateProject(path: string, patch: { name?: string; pinned?: boolean; hidden?: boolean }) {
    try {
      const project = await client().request("projects.update", { path, ...patch });
      set({ projects: get().projects.map((p) => (p.path === path ? project : p)) });
    } catch (error) {
      toast.error(errorMessage(error));
    }
  },

  /** Records an explicit choice so the default expand rule no longer applies to this project. */
  toggleProject(path: string, currentlyCollapsed: boolean) {
    const projectExpanded = { ...get().projectExpanded, [path]: currentlyCollapsed };
    savePrefs({ projectExpanded });
    set({ projectExpanded });
  },

  setSettings(settings: Settings) {
    set({ settings });
  },

  setFilters(patch: Partial<Filters>) {
    const previous = get().filters;
    const filters = { ...previous, ...patch };
    savePrefs({ filters });
    set({ filters });
    if (filters.showInternal !== previous.showInternal) void loadLists().catch((error) => toast.error(errorMessage(error)));
  },

  setDraft(key: string, text: string) {
    set({ drafts: { ...get().drafts, [key]: text } });
  },
};

function normalizePath(path: string | null) {
  return (path ?? "").replace(/[\\/]+$/, "").toLowerCase();
}

/** Threads that live in a project the user hid stay out of lists and search unless shown. */
export function useHiddenFilter() {
  const projects = useStore((s) => s.projects);
  const showHidden = useStore((s) => s.filters.showHidden);
  const hidden = new Set(projects.filter((project) => project.hidden).map((project) => normalizePath(project.path)));
  return (thread: ThreadSummary) => showHidden || !hidden.has(normalizePath(thread.cwd));
}

/** The desktop's own window, not a paired phone: only it may open folders, editors and terminals. */
export function useIsLocalDesktop() {
  return useStore((s) => s.app !== null && !s.app.remoteClient);
}
