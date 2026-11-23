import { create } from "zustand";
import { client } from "~/api/client";
import type { ConnectionStatus } from "~/api/protocol";
import type {
  AgentInfo,
  AppInfo,
  ConfigOption,
  ContentBlock,
  Item,
  Project,
  RemoteStatus,
  RequestResolution,
  ServerEvent,
  SlashCommand,
  ThreadEvent,
  ThreadSource,
  ThreadSummary,
  ThreadUsage,
} from "~/api/types";
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

export interface Filters {
  agentIds: string[];
  sources: ThreadSource[];
  showArchived: boolean;
}

interface State {
  connection: ConnectionStatus;
  app: AppInfo | null;
  agents: AgentInfo[];
  projects: Project[];
  threads: Record<string, ThreadSummary>;
  threadsLoaded: boolean;
  open: Record<string, OpenThread>;
  remote: RemoteStatus | null;
  route: Route;
  filters: Filters;
  collapsedProjects: Record<string, boolean>;
  rawPanelOpen: boolean;
  paletteOpen: boolean;
  mobileSidebarOpen: boolean;
  drafts: Record<string, string>;
}

const COLLAPSED_KEY = "vetro:collapsed-projects";
const FILTERS_KEY = "vetro:filters";

function readJson<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? { ...fallback, ...JSON.parse(raw) } : fallback;
  } catch {
    return fallback;
  }
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

export const useStore = create<State>(() => ({
  connection: "connecting",
  app: null,
  agents: [],
  projects: [],
  threads: {},
  threadsLoaded: false,
  open: {},
  remote: null,
  route: routeFromLocation(),
  filters: readJson<Filters>(FILTERS_KEY, { agentIds: [], sources: [], showArchived: false }),
  collapsedProjects: readJson<Record<string, boolean>>(COLLAPSED_KEY, {}),
  rawPanelOpen: new URLSearchParams(location.search).get("panel") === "raw",
  paletteOpen: new URLSearchParams(location.search).get("palette") === "1",
  mobileSidebarOpen: new URLSearchParams(location.search).get("sidebar") === "1",
  drafts: {},
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

function mergeSummaries(threads: ThreadSummary[]) {
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
    case "index.progress":
      break;
  }
}

async function loadLists() {
  const api = client();
  const [agents, projects] = await Promise.all([api.request("agents.list", {}), api.request("projects.list", {})]);
  set({ agents: agents.agents, projects: projects.projects });
  const threads: ThreadSummary[] = [];
  let cursor: string | null = null;
  do {
    const page: { threads: ThreadSummary[]; nextCursor: string | null } = await api.request("threads.list", {
      includeArchived: true,
      includeChildren: true,
      limit: 500,
      ...(cursor ? { cursor } : {}),
    });
    threads.push(...page.threads);
    cursor = page.nextCursor;
  } while (cursor && threads.length < 5000);
  const map: Record<string, ThreadSummary> = {};
  for (const thread of threads) map[thread.id] = thread;
  set({ threads: map, threadsLoaded: true });
}

let started = false;

export function startSync() {
  if (started) return;
  started = true;
  const api = client();
  api.onEvent(handleEvent);
  api.onStatus((status, { info, reconnected }) => {
    set({ connection: status, ...(info ? { app: info } : {}) });
    if (status !== "open") return;
    void loadLists().catch((error) => toast.error(errorMessage(error)));
    if (reconnected) for (const id of Object.keys(get().open)) void actions.openThread(id, true);
  });
  api.start();
  window.addEventListener("popstate", () => set({ route: routeFromLocation(), mobileSidebarOpen: false }));
}

const opening = new Map<string, Promise<void>>();
const refs = new Map<string, number>();

export const actions = {
  navigate(route: Route, replace = false) {
    const url = routeToUrl(route);
    if (replace) history.replaceState(null, "", url);
    else if (url !== `${location.pathname}${location.search}`) history.pushState(null, "", url);
    set({ route, mobileSidebarOpen: false });
  },

  openThread(threadId: string, force = false): Promise<void> {
    if (!force && get().open[threadId]) return Promise.resolve();
    const inflight = opening.get(threadId);
    if (inflight && !force) return inflight;
    const promise = client()
      .request("threads.open", { threadId, limit: 200 })
      .then((result) => {
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
      })
      .catch((error) => {
        toast.error(errorMessage(error));
        throw error;
      })
      .finally(() => opening.delete(threadId));
    opening.set(threadId, promise);
    return promise;
  },

  /** Keeps a thread open while at least one view shows it. */
  retain(threadId: string) {
    refs.set(threadId, (refs.get(threadId) ?? 0) + 1);
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

  async updateThread(threadId: string, patch: { title?: string; archived?: boolean }) {
    try {
      const { thread } = await client().request("threads.update", { threadId, ...patch });
      mergeSummaries([thread]);
    } catch (error) {
      toast.error(errorMessage(error));
    }
  },

  async detach(threadId: string) {
    await client().request("threads.detach", { threadId }).catch((error) => toast.error(errorMessage(error)));
  },

  async addProject(path: string) {
    const project = await client().request("projects.add", { path });
    if (!get().projects.some((p) => p.path === project.path)) set({ projects: [...get().projects, project] });
    return project;
  },

  async updateProject(path: string, patch: { name?: string; pinned?: boolean; hidden?: boolean }) {
    try {
      const project = await client().request("projects.update", { path, ...patch });
      set({ projects: get().projects.map((p) => (p.path === path ? project : p)) });
    } catch (error) {
      toast.error(errorMessage(error));
    }
  },

  toggleProject(path: string) {
    const collapsedProjects = { ...get().collapsedProjects, [path]: !get().collapsedProjects[path] };
    localStorage.setItem(COLLAPSED_KEY, JSON.stringify(collapsedProjects));
    set({ collapsedProjects });
  },

  setFilters(patch: Partial<Filters>) {
    const filters = { ...get().filters, ...patch };
    localStorage.setItem(FILTERS_KEY, JSON.stringify(filters));
    set({ filters });
  },

  setDraft(key: string, text: string) {
    set({ drafts: { ...get().drafts, [key]: text } });
  },
};
