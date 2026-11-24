import type {
  AgentInfo,
  ContentBlock,
  Item,
  ItemOf,
  Project,
  RawEntry,
  ServerEvent,
  ThreadEvent,
  ThreadSummary,
} from "~/api/types";
import {
  type AuthApi,
  type ConnectionStatus,
  type Method,
  type Params,
  type Result,
  RpcFailure,
  type StatusListener,
  type Transport,
} from "~/api/protocol";
import {
  HOME,
  type MockThread,
  agentOptions,
  agents as agentFixtures,
  buildThreads,
  fileTree,
  projectFiles,
  projects as projectFixtures,
  remote as remoteFixture,
  settings as settingsFixture,
} from "./fixtures";

type Run = { cancelled: boolean; resolveRequest?: (optionId: string) => void };

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

function mergePatch(target: unknown, patch: unknown): unknown {
  if (patch === null || typeof patch !== "object" || Array.isArray(patch)) return patch;
  const out: Record<string, unknown> =
    target && typeof target === "object" && !Array.isArray(target) ? { ...(target as Record<string, unknown>) } : {};
  for (const [key, value] of Object.entries(patch as Record<string, unknown>)) {
    if (value === null) delete out[key];
    else out[key] = mergePatch(out[key], value);
  }
  return out;
}

function textOf(item: Item): string {
  switch (item.type) {
    case "user_message":
      return item.content.map((block) => (block.type === "text" ? block.text : "")).join(" ");
    case "agent_message":
    case "reasoning":
      return item.text;
    default:
      return "";
  }
}

const remoteClient = new URLSearchParams(location.search).has("remote");
/** Agents that report options only from a live session, like ACP agents without a cache. */
const SESSION_OPTIONS = new Set(["devin"]);

function escapeHtml(text: string) {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/** Codex-like dependency: the small model has fewer reasoning levels. */
function withSelection(options: { options: Result<"agents.options">["options"]; commands: Result<"agents.options">["commands"] }, selected: Record<string, string>) {
  const model = selected.model;
  const result = structuredClone(options);
  for (const option of result.options) {
    if (selected[option.id]) option.currentValue = selected[option.id]!;
    if (option.category === "thought_level" && model?.includes("mini")) {
      option.choices = option.choices.filter((choice) => choice.value === "low" || choice.value === "medium");
      if (!option.choices.some((choice) => choice.value === option.currentValue)) option.currentValue = "medium";
    }
  }
  return result;
}

const LIVE_USAGE = { input: 14_200, cachedInput: 9_800, output: 420, reasoning: 120, contextUsed: 14_620, contextWindow: 272_000 };

/** The whole Vetro protocol, in the browser, over fixtures. */
export class MockTransport implements Transport {
  private eventListeners = new Set<(event: ServerEvent) => void>();
  private statusListeners = new Set<StatusListener>();
  private current: ConnectionStatus = "closed";
  private threads = new Map<string, MockThread>();
  private seq = new Map<string, number>();
  private opened = new Set<string>();
  private agents: AgentInfo[] = structuredClone(agentFixtures);
  private projects: Project[] = structuredClone(projectFixtures);
  private settings = structuredClone(settingsFixture);
  private remote = structuredClone(remoteFixture);
  private nextOrdinal = new Map<string, number>();
  private runs = new Map<string, Run>();
  private counter = 0;
  private prepared = new Set<string>();
  private prefs: unknown = null;

  constructor() {
    for (const thread of buildThreads()) this.threads.set(thread.summary.id, thread);
    this.refreshProjectCounts();
  }

  start() {
    this.current = "connecting";
    for (const listener of this.statusListeners) listener("connecting", { info: null, reconnected: false });
    setTimeout(() => {
      this.current = "open";
      const info = this.handlers["app.hello"]({ clientName: "vetro-web", protocolVersion: 1 }) as Result<"app.hello">;
      for (const listener of this.statusListeners) listener("open", { info, reconnected: false });
    }, 20);
  }

  status() {
    return this.current;
  }

  onEvent(listener: (event: ServerEvent) => void) {
    this.eventListeners.add(listener);
    return () => {
      this.eventListeners.delete(listener);
    };
  }

  onStatus(listener: StatusListener) {
    this.statusListeners.add(listener);
    return () => {
      this.statusListeners.delete(listener);
    };
  }

  async request<M extends Method>(method: M, params: Params<M>): Promise<Result<M>> {
    await sleep(10 + Math.random() * 40);
    const handler = this.handlers[method] as unknown as ((params: Params<M>) => Result<M> | Promise<Result<M>>) | undefined;
    if (!handler) throw new RpcFailure("bad_request", `Unknown method ${method}`);
    return structuredClone(await handler(structuredClone(params)));
  }

  private emit(event: ServerEvent) {
    const copy = structuredClone(event);
    for (const listener of this.eventListeners) listener(copy);
  }

  private threadEvent(threadId: string, event: ThreadEvent) {
    const seq = (this.seq.get(threadId) ?? 0) + 1;
    this.seq.set(threadId, seq);
    if (this.opened.has(threadId)) this.emit({ event: "thread.event", data: { threadId, seq, event } });
  }

  private thread(threadId: string) {
    const thread = this.threads.get(threadId);
    if (!thread) throw new RpcFailure("not_found", `No thread ${threadId}`);
    return thread;
  }

  private refreshProjectCounts() {
    for (const project of this.projects) {
      const threads = [...this.threads.values()].filter((t) => t.summary.cwd === project.path && !t.summary.parent);
      project.threadCount = threads.length;
      project.lastActivityAt = threads.reduce<number | null>((max, t) => Math.max(max ?? 0, t.summary.updatedAt), null);
      const roots = threads.filter((t) => !t.summary.internal);
      project.rootThreadCount = roots.length;
      project.lastRootActivityAt = roots.reduce<number | null>((max, t) => Math.max(max ?? 0, t.summary.updatedAt), null);
    }
  }

  private updateSummary(thread: MockThread, patch: Partial<ThreadSummary>) {
    Object.assign(thread.summary, patch, { updatedAt: Date.now() });
    this.threadEvent(thread.summary.id, { type: "summary", thread: thread.summary });
    this.emit({ event: "threads.upserted", data: { threads: [thread.summary] } });
  }

  private ordinal(thread: MockThread) {
    const next = (this.nextOrdinal.get(thread.summary.id) ?? thread.items.at(-1)?.ordinal ?? 0) + 1;
    this.nextOrdinal.set(thread.summary.id, next);
    return next;
  }

  private addItem<T extends Item>(thread: MockThread, item: T) {
    thread.items.push(item);
    this.threadEvent(thread.summary.id, { type: "item_added", item });
    return item;
  }

  private updateItem(thread: MockThread, item: Item) {
    const index = thread.items.findIndex((existing) => existing.id === item.id);
    if (index >= 0) thread.items[index] = item;
    this.threadEvent(thread.summary.id, { type: "item_updated", item });
  }

  private raw(thread: MockThread, direction: RawEntry["direction"], text: string) {
    thread.raw.push({ ts: Date.now(), direction, text });
  }

  private base(thread: MockThread, turnId: string) {
    return { id: `live-${++this.counter}`, ordinal: this.ordinal(thread), ts: Date.now(), turnId };
  }

  private async stream(thread: MockThread, item: ItemOf<"agent_message"> | ItemOf<"reasoning">, text: string, run: Run) {
    for (const word of text.split(/(?<=\s)/)) {
      if (run.cancelled) break;
      await sleep(18 + Math.random() * 30);
      item.text += word;
      this.threadEvent(thread.summary.id, { type: "text_delta", itemId: item.id, delta: word });
    }
    item.streaming = false;
    this.updateItem(thread, { ...item });
    this.raw(thread, "received", JSON.stringify({ method: "item/completed", params: { item: { type: item.type, text: item.text } } }));
  }

  private async runTurn(thread: MockThread, content: ContentBlock[]) {
    const run: Run = { cancelled: false };
    this.runs.set(thread.summary.id, run);
    const turnId = `turn-${++this.counter}`;
    const started = Date.now();
    const prompt = content
      .map((block) => (block.type === "text" ? block.text : block.type === "file" ? `@${block.path}` : "[image]"))
      .join(" ");
    this.raw(thread, "sent", JSON.stringify({ method: "turn/start", params: { threadId: thread.summary.nativeId, input: content } }));
    this.addItem(thread, { ...this.base(thread, turnId), type: "user_message", content });
    this.updateSummary(thread, { status: "running", live: true, preview: prompt.slice(0, 80) });

    const finish = (stopReason: ItemOf<"turn_end">["stopReason"]) => {
      this.addItem(thread, { ...this.base(thread, turnId), type: "turn_end", stopReason, durationMs: Date.now() - started, usage: LIVE_USAGE });
      thread.usage = { ...thread.usage, lastTurn: LIVE_USAGE };
      this.threadEvent(thread.summary.id, { type: "usage", usage: thread.usage });
      this.updateSummary(thread, { status: "idle" });
      this.runs.delete(thread.summary.id);
    };

    await sleep(300);
    const reasoning = this.addItem(thread, { ...this.base(thread, turnId), type: "reasoning", text: "", streaming: true });
    await this.stream(thread, reasoning, "The user wants a quick check. I'll look at the working tree first, then answer briefly.", run);
    if (run.cancelled) return finish("cancelled");

    const terminal = { type: "terminal" as const, command: "git status --short", cwd: thread.summary.cwd };
    const call = this.addItem(thread, {
      ...this.base(thread, turnId),
      type: "tool_call",
      callId: `call-live-${this.counter}`,
      kind: "execute",
      name: "shell",
      title: "git status --short",
      status: "in_progress",
      input: { command: ["git", "status", "--short"] },
      output: null,
      content: [{ ...terminal, output: "", exitCode: null }],
      locations: [],
      childThreadIds: [],
    } satisfies ItemOf<"tool_call">);
    await sleep(900);
    if (run.cancelled) return finish("cancelled");
    this.updateItem(thread, {
      ...call,
      status: "completed",
      content: [{ ...terminal, output: " M apps/web/src/App.tsx\n?? apps/web/src/components/RawLogPanel.tsx", exitCode: 0 }],
    });

    const message = this.addItem(thread, { ...this.base(thread, turnId), type: "agent_message", text: "", streaming: true });
    await this.stream(
      thread,
      message,
      "Two files changed: `App.tsx` is modified and `RawLogPanel.tsx` is new. To finish I need to run the build, which writes to `apps/web/dist`.",
      run,
    );
    if (run.cancelled) return finish("cancelled");

    await sleep(250);
    const request = this.addItem(thread, {
      ...this.base(thread, turnId),
      type: "request",
      requestId: `req-live-${this.counter}`,
      callId: null,
      title: "Run pnpm --dir apps/web build?",
      detail: "pnpm --dir apps/web build",
      body: {
        type: "permission",
        options: [
          { id: "allow", label: "Allow once", kind: "allow_once" },
          { id: "always", label: "Always allow pnpm", kind: "allow_always" },
          { id: "reject", label: "Reject", kind: "reject_once" },
        ],
      },
      resolution: null,
    } satisfies ItemOf<"request">);
    this.updateSummary(thread, { status: "awaiting_input" });
    const optionId = await new Promise<string>((resolve) => {
      run.resolveRequest = resolve;
    });
    run.resolveRequest = undefined;
    this.updateItem(thread, { ...request, resolution: run.cancelled ? { type: "cancelled" } : { type: "selected", optionId } });
    if (run.cancelled) return finish("cancelled");
    this.updateSummary(thread, { status: "running" });

    const reply = this.addItem(thread, { ...this.base(thread, turnId), type: "agent_message", text: "", streaming: true });
    await this.stream(
      thread,
      reply,
      optionId === "reject"
        ? "Understood, I won't run the build. The changes are ready for you to review."
        : "The build finished without warnings. Everything is ready.",
      run,
    );
    finish(run.cancelled ? "cancelled" : "end_turn");
  }

  private desktopOnly() {
    if (remoteClient) throw new RpcFailure("unauthorized", "Local authentication required");
  }

  private startThread(params: Params<"threads.start">): Result<"threads.start"> {
    if (remoteClient && !this.projects.some((p) => p.path === params.cwd))
      throw new RpcFailure("unauthorized", "Remote devices can only start threads in known project folders");
    const agent = this.agents.find((a) => a.id === params.agentId);
    if (!agent?.installed) throw new RpcFailure("agent_unavailable", agent?.error ?? "Agent not available");
    if (agent.auth.state === "required") throw new RpcFailure("auth_required", `${agent.name} needs you to sign in first`);
    const nativeId = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
    const id = `${params.agentId}:${nativeId}`;
    const base = agentOptions[params.agentId] ?? { options: [], commands: [] };
    const options = structuredClone(base.options).map((option) => {
      const value = params.options[option.id];
      return value ? { ...option, currentValue: value } : option;
    });
    const text = params.content.find((block) => block.type === "text");
    const title = text?.type === "text" ? text.text.split("\n")[0]!.slice(0, 60) : "New thread";
    const model = options.find((o) => o.category === "model");
    const launch = agent.kind === "acp" ? "acp" : agent.kind === "codex" ? "app-server" : "-p --input-format stream-json --output-format stream-json --verbose";
    const thread: MockThread = {
      summary: {
        id,
        agentId: params.agentId,
        nativeId,
        title,
        preview: null,
        cwd: params.cwd,
        createdAt: Date.now(),
        updatedAt: Date.now(),
        source: "vetro",
        origin: "vetro",
        status: "running",
        live: true,
        resumable: true,
        parent: null,
        childCount: 0,
        model: model?.currentValue ?? null,
        archived: false,
        internal: false,
      },
      items: [],
      options,
      commands: base.commands,
      usage: { lastTurn: null, total: null, rateLimits: [] },
      raw: [{ ts: Date.now(), direction: "spawn", text: `"${agent.path}" ${launch}   (cwd ${params.cwd}; env overrides: none)` }],
    };
    this.threads.set(id, thread);
    this.opened.add(id);
    if (!this.projects.some((p) => p.path === params.cwd)) this.handlers["projects.add"]({ path: params.cwd });
    this.refreshProjectCounts();
    this.emit({ event: "threads.upserted", data: { threads: [thread.summary] } });
    this.emit({ event: "projects.updated", data: { projects: this.projects } });
    setTimeout(() => void this.runTurn(thread, params.content), 50);
    return { thread: thread.summary };
  }

  private respond(params: Params<"threads.respond">): Result<"threads.respond"> {
    const thread = this.thread(params.threadId);
    const run = this.runs.get(params.threadId);
    const request = thread.items.find(
      (item): item is ItemOf<"request"> => item.type === "request" && item.requestId === params.requestId,
    );
    if (!request) throw new RpcFailure("not_found", "No such request");
    this.raw(thread, "sent", JSON.stringify({ requestId: params.requestId, result: params.resolution }));
    if (run?.resolveRequest && params.resolution.type === "selected") {
      run.resolveRequest(params.resolution.optionId);
      return {};
    }
    this.updateItem(thread, { ...request, resolution: params.resolution });
    const turnId = request.turnId ?? "t";
    const resolution = params.resolution;
    const rejected = resolution.type === "cancelled" || (resolution.type === "selected" && resolution.optionId.startsWith("reject"));
    const reply = rejected
      ? "Understood, I left everything as it was."
      : resolution.type === "answered"
        ? `Thanks. Going with ${Object.values(resolution.answers).flat().join(", ") || "your answers"}.`
        : "Done. The folders are gone and the release build finished in 1m 42s.";
    if (!rejected && request.callId) {
      const call = thread.items.find((item): item is ItemOf<"tool_call"> => item.type === "tool_call" && item.callId === request.callId);
      if (call) this.updateItem(thread, { ...call, status: "completed" });
    }
    this.addItem(thread, { ...this.base(thread, turnId), type: "agent_message", text: reply, streaming: false });
    this.addItem(thread, { ...this.base(thread, turnId), type: "turn_end", stopReason: "end_turn", usage: null, durationMs: 4200 });
    this.updateSummary(thread, { status: "idle" });
    return {};
  }

  private search(params: Params<"search.threads">): Result<"search.threads"> {
    const query = params.query.trim().toLowerCase();
    if (!query) return { hits: [] };
    const hits: Result<"search.threads">["hits"] = [];
    const hidden = new Set(this.projects.filter((p) => p.hidden).map((p) => p.path));
    for (const thread of this.threads.values()) {
      if (!params.includeHidden && thread.summary.cwd && hidden.has(thread.summary.cwd)) continue;
      for (const source of [thread.summary.title, ...thread.items.map(textOf)]) {
        const index = source.toLowerCase().indexOf(query);
        if (index < 0) continue;
        const from = Math.max(0, index - 40);
        const snippet = `${from > 0 ? "…" : ""}${escapeHtml(source.slice(from, index))}<mark>${escapeHtml(source.slice(index, index + query.length))}</mark>${escapeHtml(source.slice(index + query.length, index + query.length + 60))}`;
        hits.push({ thread: thread.summary, snippet });
        break;
      }
    }
    hits.sort((a, b) => b.thread.updatedAt - a.thread.updatedAt);
    return { hits: hits.slice(0, params.limit ?? 20) };
  }

  private list(params: Params<"fs.list">): Result<"fs.list"> {
    const path = params.path ?? HOME;
    const key = Object.keys(fileTree).find((k) => k.toLowerCase() === path.toLowerCase()) ?? path;
    const names =
      fileTree[key] ?? (this.projects.some((p) => p.path === key) ? ["src", "docs", "README.md", "package.json"] : []);
    const join = (name: string) => (key === "" ? name : key.endsWith("\\") ? `${key}${name}` : `${key}\\${name}`);
    const entries = names
      .map((name) => ({ name: name.replace(/\\$/, ""), path: join(name), isDir: !/\.\w+$/.test(name) }))
      .sort((a, b) => Number(b.isDir) - Number(a.isDir) || a.name.localeCompare(b.name));
    const parts = key.replace(/\\$/, "").split("\\");
    const parent = key === "" ? null : parts.length <= 1 ? "" : parts.length === 2 ? `${parts[0]}\\` : parts.slice(0, -1).join("\\");
    return { path: key, parent, entries };
  }

  private find(params: Params<"fs.find">): Result<"fs.find"> {
    const query = params.query.toLowerCase();
    const scored = projectFiles
      .map((path) => {
        const lower = path.toLowerCase();
        let position = 0;
        for (const char of query) {
          position = lower.indexOf(char, position);
          if (position < 0) return null;
          position += 1;
        }
        const name = lower.split("/").at(-1)!;
        return { path, score: (name.includes(query) ? 0 : 10) + path.length / 100 };
      })
      .filter((entry): entry is { path: string; score: number } => entry !== null)
      .sort((a, b) => a.score - b.score);
    return { paths: scored.slice(0, params.limit ?? 20).map((entry) => entry.path) };
  }

  private handlers: { [M in Method]: (params: Params<M>) => Result<M> | Promise<Result<M>> } = {
    "app.hello": () => ({
      version: "0.1.0-mock",
      protocolVersion: 1,
      platform: "windows",
      hostname: "CHRIS-DESK",
      homeDir: HOME,
      dataDir: `${HOME}\\.vetro`,
      remoteClient,
    }),
    "agents.list": () => ({ agents: this.agents }),
    "agents.refresh": () => {
      this.emit({ event: "agents.updated", data: { agents: this.agents } });
      return { agents: this.agents };
    },
    "agents.authenticate": async (params) => {
      this.desktopOnly();
      await sleep(1200);
      const agent = this.agents.find((a) => a.id === params.agentId);
      if (!agent) throw new RpcFailure("not_found", "Unknown agent");
      if (params.methodId === "device-code") {
        setTimeout(() => {
          agent.auth = { ...agent.auth, state: "ok", account: "Google AI Pro" };
          this.emit({ event: "agents.updated", data: { agents: this.agents } });
        }, 6000);
        return { message: "Open the link on any device and enter the code.", url: "https://accounts.example.com/device", code: "WDJB-MJHT" };
      }
      agent.auth = { ...agent.auth, state: "ok", account: "Google AI Pro" };
      this.emit({ event: "agents.updated", data: { agents: this.agents } });
      return { message: null, url: null, code: null };
    },
    "agents.options": (params) => {
      const options = agentOptions[params.agentId];
      if (!options) throw new RpcFailure("agent_unavailable", "This agent is not available");
      if (SESSION_OPTIONS.has(params.agentId) && !this.prepared.has(params.agentId)) {
        if (!params.prepare) return { options: [], commands: [], needsSession: true };
        this.prepared.add(params.agentId);
      }
      return { ...withSelection(options, params.selected ?? {}), needsSession: false };
    },
    "projects.list": () => ({ projects: this.projects }),
    "projects.add": (params) => {
      this.desktopOnly();
      const existing = this.projects.find((p) => p.path.toLowerCase() === params.path.toLowerCase());
      if (existing) return existing;
      const name = params.path.split(/[\\/]/).filter(Boolean).at(-1) ?? params.path;
      const project: Project = { path: params.path, name, exists: true, pinned: false, hidden: false, threadCount: 0, lastActivityAt: Date.now(), rootThreadCount: 0, lastRootActivityAt: null };
      this.projects.push(project);
      this.emit({ event: "projects.updated", data: { projects: this.projects } });
      return project;
    },
    "projects.remove": (params) => {
      this.desktopOnly();
      const index = this.projects.findIndex((p) => p.path === params.path);
      if (index < 0) throw new RpcFailure("not_found", "Unknown project");
      if ([...this.threads.values()].some((t) => t.summary.cwd === params.path)) return {};
      this.projects.splice(index, 1);
      this.emit({ event: "projects.updated", data: { projects: this.projects } });
      return {};
    },
    "projects.update": (params) => {
      const project = this.projects.find((p) => p.path === params.path);
      if (!project) throw new RpcFailure("not_found", "Unknown project");
      if (params.name !== undefined) project.name = params.name;
      if (params.pinned !== undefined) project.pinned = params.pinned;
      if (params.hidden !== undefined) project.hidden = params.hidden;
      this.emit({ event: "projects.updated", data: { projects: this.projects } });
      return project;
    },
    "threads.list": (params) => {
      let list = [...this.threads.values()].map((t) => t.summary);
      if (params.projectPath) list = list.filter((t) => t.cwd === params.projectPath);
      if (params.agentIds?.length) list = list.filter((t) => params.agentIds!.includes(t.agentId));
      if (params.sources?.length) list = list.filter((t) => params.sources!.includes(t.source));
      if (params.parentId) list = list.filter((t) => t.parent?.threadId === params.parentId);
      else if (!params.includeChildren) list = list.filter((t) => !t.parent);
      if (!params.includeArchived) list = list.filter((t) => !t.archived);
      if (!params.includeInternal) list = list.filter((t) => !t.internal);
      list.sort((a, b) => b.updatedAt - a.updatedAt);
      const offset = params.cursor ? Number(params.cursor) : 0;
      const limit = params.limit ?? 200;
      return { threads: list.slice(offset, offset + limit), nextCursor: offset + limit < list.length ? String(offset + limit) : null };
    },
    "threads.open": (params) => {
      const thread = this.thread(params.threadId);
      this.opened.add(params.threadId);
      const items = thread.items.slice(-(params.limit ?? 200));
      return {
        thread: thread.summary,
        items,
        hasMore: thread.items.length > items.length,
        seq: this.seq.get(params.threadId) ?? 0,
        options: thread.options,
        commands: thread.commands,
        usage: thread.usage,
      };
    },
    "threads.close": (params) => {
      this.opened.delete(params.threadId);
      return {};
    },
    "threads.items": (params) => {
      const older = this.thread(params.threadId).items.filter((item) => item.ordinal < params.before);
      const items = older.slice(-(params.limit ?? 200));
      return { items, hasMore: older.length > items.length };
    },
    "threads.start": (params) => this.startThread(params),
    "threads.send": (params) => {
      const thread = this.thread(params.threadId);
      if (!thread.summary.resumable) throw new RpcFailure("not_resumable", "This session is read-only history");
      if (this.runs.has(params.threadId)) throw new RpcFailure("busy", "A turn is already running");
      setTimeout(() => void this.runTurn(thread, params.content), 10);
      return {};
    },
    "threads.interrupt": (params) => {
      const run = this.runs.get(params.threadId);
      if (run) {
        run.cancelled = true;
        run.resolveRequest?.("reject");
      }
      return {};
    },
    "threads.setOption": (params) => {
      const thread = this.thread(params.threadId);
      thread.options = thread.options.map((o) => (o.id === params.optionId ? { ...o, currentValue: params.value } : o));
      this.threadEvent(params.threadId, { type: "options", options: thread.options });
      return { options: thread.options };
    },
    "threads.respond": (params) => this.respond(params),
    "threads.update": (params) => {
      const thread = this.thread(params.threadId);
      if (params.title !== undefined) thread.summary.title = params.title;
      if (params.archived !== undefined) thread.summary.archived = params.archived;
      this.threadEvent(params.threadId, { type: "summary", thread: thread.summary });
      this.emit({ event: "threads.upserted", data: { threads: [thread.summary] } });
      return { thread: thread.summary };
    },
    "threads.detach": (params) => {
      this.updateSummary(this.thread(params.threadId), { live: false });
      return {};
    },
    "threads.raw": (params) => ({ entries: this.thread(params.threadId).raw.slice(-(params.limit ?? 2000)) }),
    "search.threads": (params) => this.search(params),
    "fs.list": (params) => this.list(params),
    "fs.find": (params) => this.find(params),
    "settings.get": () => this.settings,
    "settings.update": (params) => {
      this.desktopOnly();
      this.settings = mergePatch(this.settings, params.patch) as typeof this.settings;
      return this.settings;
    },
    "prefs.get": () => ({ prefs: (this.prefs ?? null) as never }),
    "prefs.set": (params) => {
      this.prefs = params.prefs;
      return {};
    },
    "remote.status": () => this.remote,
    "remote.set": (params) => {
      this.desktopOnly();
      this.remote.enabled = params.enabled;
      const dns = this.remote.tailscale.dnsName;
      this.remote.tailscale.serveUrl = params.tailscaleServe && dns ? `https://${dns}` : null;
      this.emit({ event: "remote.updated", data: { status: this.remote } });
      return this.remote;
    },
    "remote.createPairing": () => {
      this.desktopOnly();
      const code = "K7M-4QX";
      const serve = this.remote.tailscale.serveUrl;
      return { code, url: serve ? `${serve}/?code=${code}` : null, expiresAt: Date.now() + 5 * 60_000 };
    },
    "remote.revokeDevice": (params) => {
      this.remote.devices = this.remote.devices.filter((d) => d.id !== params.deviceId);
      this.emit({ event: "remote.updated", data: { status: this.remote } });
      return this.remote;
    },
  };
}

export const mockAuth: AuthApi = {
  async session() {
    await sleep(10);
    if (new URLSearchParams(location.search).has("pair")) return null;
    return { authenticated: true, remoteClient: false };
  },
  async local() {
    return { ok: true };
  },
  async pair(code) {
    await sleep(500);
    return code.replace(/[^a-z0-9]/gi, "").toUpperCase() === "K7M4QX"
      ? { ok: true }
      : { ok: false, message: "That code is not valid or has expired." };
  },
};
