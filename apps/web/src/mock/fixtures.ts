import type {
  AgentInfo,
  ConfigOption,
  ContentBlock,
  Item,
  ItemOf,
  PlanEntry,
  Project,
  RawEntry,
  RemoteStatus,
  Settings,
  SlashCommand,
  ThreadSummary,
  ThreadUsage,
  TokenUsage,
  ToolCall,
} from "~/api/types";

export const HOME = "C:\\Users\\Alex";
const now = Date.now();
const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;

export const agents: AgentInfo[] = [
  {
    id: "claude",
    name: "Claude Code",
    kind: "claude",
    installed: true,
    path: `${HOME}\\.local\\bin\\claude.exe`,
    version: "2.1.31",
    auth: { state: "ok", methods: [], account: "Claude Pro" },
    capabilities: { imageInput: true, midTurnInput: false, resume: true, listSessions: true, fork: { whole: true, atTurn: false } },
    error: null,
  },
  {
    id: "codex",
    name: "Codex",
    kind: "codex",
    installed: true,
    path: `${HOME}\\AppData\\Roaming\\npm\\node_modules\\@openai\\codex\\vendor\\x86_64-pc-windows-msvc\\codex\\codex.exe`,
    version: "0.98.0",
    auth: { state: "ok", methods: [], account: "ChatGPT Plus", provider: "litellm", endpoint: "http://127.0.0.1:4000" },
    capabilities: { imageInput: true, midTurnInput: true, resume: true, listSessions: true, fork: { whole: true, atTurn: true } },
    error: null,
  },
  {
    id: "antigravity",
    name: "Antigravity",
    kind: "acp",
    installed: true,
    path: `${HOME}\\AppData\\Local\\agy\\bin\\agy.exe`,
    version: "1.4.2",
    auth: {
      state: "required",
      methods: [
        { id: "oauth-personal", name: "Sign in with Google", description: "Opens Google sign-in in your browser." },
        { id: "device-code", name: "Sign in with a code", description: "Finish on any device with a browser." },
        { id: "gemini-api-key", name: "Use GEMINI_API_KEY", description: "Reads the key from the agent's environment." },
      ],
      account: null,
    },
    capabilities: { imageInput: true, midTurnInput: false, resume: true, listSessions: true, fork: { whole: true, atTurn: false } },
    error: null,
  },
  {
    id: "devin",
    name: "Devin",
    kind: "acp",
    installed: true,
    path: "D:\\Software\\Devin\\resources\\app\\extensions\\windsurf\\devin\\bin\\devin.exe",
    version: "2026.11.4",
    auth: { state: "ok", methods: [{ id: "devin-browser", name: "Sign in with browser", description: null }], account: "Devin Max" },
    capabilities: { imageInput: true, midTurnInput: false, resume: true, listSessions: true, fork: { whole: true, atTurn: false } },
    error: null,
  },
  {
    id: "goose",
    name: "Goose",
    kind: "acp",
    installed: false,
    path: null,
    version: null,
    auth: { state: "unknown", methods: [], account: null },
    capabilities: { imageInput: false, midTurnInput: false, resume: false, listSessions: false, fork: { whole: false, atTurn: false } },
    error: "Command not found: goose",
  },
];

function choice(value: string, name: string, description: string | null = null, group: string | null = null, danger = false) {
  return { value, name, description, group, danger };
}

export const agentOptions: Record<string, { options: ConfigOption[]; commands: SlashCommand[] }> = {
  codex: {
    options: [
      {
        id: "model",
        name: "Model",
        description: null,
        category: "model",
        currentValue: "gpt-5.5-codex",
        choices: [
          choice("gpt-5.5-codex", "GPT-5.5-Codex", "Optimized for agentic coding"),
          choice("gpt-5.5", "GPT-5.5", "Broad world knowledge"),
          choice("gpt-5.4-mini", "GPT-5.4-Mini", "Fast and cheap"),
        ],
      },
      {
        id: "reasoning_effort",
        name: "Reasoning",
        description: null,
        category: "thought_level",
        currentValue: "medium",
        choices: [choice("low", "Low"), choice("medium", "Medium"), choice("high", "High"), choice("xhigh", "Extra high")],
      },
      {
        id: "approval_preset",
        name: "Access",
        description: null,
        category: "mode",
        currentValue: "auto",
        choices: [
          choice("read-only", "Read only", "Asks before editing files or running commands"),
          choice("auto", "Auto", "Edits and runs commands in the workspace"),
          choice("full-access", "Full access", "No sandbox, no approvals", null, true),
        ],
      },
      {
        id: "sandbox",
        name: "Sandbox",
        description: null,
        category: "other",
        currentValue: "workspace-write",
        choices: [
          choice("read-only", "Read only", "The agent can only read files"),
          choice("workspace-write", "Workspace write", "Edits stay inside the workspace"),
          choice("danger-full-access", "Full access", "No sandbox", null, true),
        ],
      },
    ],
    commands: [
      { name: "review", description: "Review my current changes", inputHint: null },
      { name: "compact", description: "Summarize the conversation to free context", inputHint: null },
      { name: "init", description: "Create an AGENTS.md file", inputHint: null },
      { name: "status", description: "Show session configuration and usage", inputHint: null },
    ],
  },
  claude: {
    options: [
      {
        id: "model",
        name: "Model",
        description: null,
        category: "model",
        currentValue: "default",
        choices: [
          choice("default", "Default", "Recommended for your plan"),
          choice("opus", "Opus", "Most capable", "Claude 5"),
          choice("sonnet", "Sonnet", "Fast and capable", "Claude 5"),
          choice("haiku", "Haiku", "Fastest", "Claude 4.5"),
        ],
      },
      {
        id: "permission_mode",
        name: "Mode",
        description: null,
        category: "mode",
        currentValue: "default",
        choices: [
          choice("default", "Ask before edits"),
          choice("acceptEdits", "Accept edits"),
          choice("plan", "Plan mode"),
        ],
      },
    ],
    commands: [
      { name: "compact", description: "Clear history but keep a summary", inputHint: "[instructions]" },
      { name: "review", description: "Review a pull request", inputHint: null },
      { name: "context", description: "Visualize current context usage", inputHint: null },
      { name: "cost", description: "Show token usage for this session", inputHint: null },
      { name: "security-review", description: "Review pending changes for security issues", inputHint: null },
    ],
  },
  antigravity: {
    options: [
      {
        id: "model",
        name: "Model",
        description: null,
        category: "model",
        currentValue: "gemini-3-pro",
        choices: [choice("gemini-3-pro", "Gemini 3 Pro"), choice("gemini-3-flash", "Gemini 3 Flash")],
      },
      {
        id: "mode",
        name: "Mode",
        description: null,
        category: "mode",
        currentValue: "default",
        choices: [choice("default", "Default"), choice("auto_edit", "Auto edit"), choice("plan", "Plan")],
      },
    ],
    commands: [
      { name: "memory", description: "Manage memory", inputHint: null },
      { name: "tools", description: "List available tools", inputHint: null },
    ],
  },
  devin: {
    options: [
      {
        id: "model",
        name: "Model",
        description: null,
        category: "model",
        currentValue: "claude-opus-5-5",
        choices: [
          choice("claude-opus-5-5", "Claude Opus 5.5"),
          choice("gpt-5-5", "GPT-5.5"),
          choice("swe-2", "SWE-2"),
        ],
      },
      {
        id: "thinking",
        name: "Thinking",
        description: null,
        category: "thought_level",
        currentValue: "high",
        choices: [choice("off", "Off"), choice("medium", "Medium"), choice("high", "High")],
      },
      {
        id: "mode",
        name: "Mode",
        description: null,
        category: "mode",
        currentValue: "code",
        choices: [choice("code", "Code"), choice("plan", "Plan"), choice("ask", "Ask")],
      },
    ],
    commands: [
      { name: "plan", description: "Switch to plan mode", inputHint: null },
      { name: "review", description: "Review the working tree", inputHint: null },
      { name: "init", description: "Create AGENTS.md", inputHint: null },
    ],
  },
};

export const projects: Project[] = [
  { path: "D:\\Dev\\Projects\\vetro", name: "vetro", exists: true, pinned: true, hidden: false, threadCount: 0, lastActivityAt: null, rootThreadCount: 0, lastRootActivityAt: null },
  { path: "D:\\Dev\\Projects\\atlas-api", name: "atlas-api", exists: true, pinned: false, hidden: false, threadCount: 0, lastActivityAt: null, rootThreadCount: 0, lastRootActivityAt: null },
  { path: "D:\\Dev\\Projects\\field-notes", name: "field-notes", exists: true, pinned: false, hidden: false, threadCount: 0, lastActivityAt: null, rootThreadCount: 0, lastRootActivityAt: null },
  { path: `${HOME}\\scratch\\tauri-spike`, name: "tauri-spike", exists: false, pinned: false, hidden: false, threadCount: 0, lastActivityAt: null, rootThreadCount: 0, lastRootActivityAt: null },
  { path: "D:\\Dev\\Sandbox\\tmp.scratch-run", name: "tmp.scratch-run", exists: true, pinned: false, hidden: false, threadCount: 0, lastActivityAt: null, rootThreadCount: 0, lastRootActivityAt: null },
  { path: `${HOME}\\Documents\\Codex\\2026-11-20\\regex-help`, name: "regex-help", exists: true, pinned: false, hidden: false, threadCount: 0, lastActivityAt: null, rootThreadCount: 0, lastRootActivityAt: null },
];

const QUICK = projects[5]!.path;

const VETRO = projects[0]!.path;
const ATLAS = projects[1]!.path;
const NOTES = projects[2]!.path;
const SPIKE = projects[3]!.path;
const SCRATCH = projects[4]!.path;

export interface MockThread {
  summary: ThreadSummary;
  items: Item[];
  options: ConfigOption[];
  commands: SlashCommand[];
  usage: ThreadUsage;
  raw: RawEntry[];
}

function usage(input: number, output: number, window = 272_000): TokenUsage {
  return {
    input,
    cachedInput: Math.round(input * 0.7),
    output,
    reasoning: Math.round(output * 0.4),
    contextUsed: input + output,
    contextWindow: window,
  };
}

class Builder {
  items: Item[] = [];
  private ordinal = 0;
  private turn = 0;
  private callSeq = 0;
  constructor(
    private ts: number,
    private readonly prefix: string,
  ) {}

  private base(step = 4000) {
    this.ts += step;
    this.ordinal += 1;
    return { id: `${this.prefix}-${this.ordinal}`, ordinal: this.ordinal, ts: this.ts, turnId: `${this.prefix}-t${this.turn}` };
  }

  user(text: string, extra: ContentBlock[] = []) {
    this.turn += 1;
    this.items.push({ ...this.base(30_000), type: "user_message", content: [{ type: "text", text }, ...extra] });
    return this;
  }

  reasoning(text: string) {
    this.items.push({ ...this.base(), type: "reasoning", text, streaming: false });
    return this;
  }

  agent(text: string) {
    this.items.push({ ...this.base(), type: "agent_message", text, streaming: false });
    return this;
  }

  tool(call: Partial<ToolCall> & Pick<ToolCall, "kind" | "title">) {
    this.callSeq += 1;
    const item: ItemOf<"tool_call"> = {
      ...this.base(6000),
      type: "tool_call",
      callId: call.callId ?? `call_${this.prefix}_${this.callSeq}`,
      name: null,
      status: "completed",
      input: null,
      output: null,
      content: [],
      locations: [],
      childThreadIds: [],
      ...call,
    };
    this.items.push(item);
    return this;
  }

  plan(entries: PlanEntry[]) {
    this.items.push({ ...this.base(), type: "plan", entries });
    return this;
  }

  request(request: Omit<ItemOf<"request">, "id" | "ordinal" | "ts" | "turnId" | "type">) {
    this.items.push({ ...this.base(), type: "request", ...request });
    return this;
  }

  notice(level: "info" | "warning" | "error", text: string) {
    this.items.push({ ...this.base(1000), type: "notice", level, text, detail: null });
    return this;
  }

  end(durationMs: number, tokens: TokenUsage | null, stopReason: ItemOf<"turn_end">["stopReason"] = "end_turn") {
    this.items.push({ ...this.base(500), type: "turn_end", stopReason, usage: tokens, durationMs });
    return this;
  }

  get lastTs() {
    return this.ts;
  }
}

const RAW_LOG_PANEL_DIFF = `@@ -1,9 +1,14 @@
 import { useThreadStore } from "~/state/store";
+import { RawLogPanel } from "./RawLogPanel";
 
 export function ThreadView({ threadId }: { threadId: string }) {
   const thread = useThreadStore((s) => s.threads[threadId]);
+  const rawOpen = useThreadStore((s) => s.rawPanelOpen);
   return (
-    <main className="flex min-h-0 flex-1 flex-col">
-      <Timeline thread={thread} />
-    </main>
+    <div className="flex min-h-0 flex-1">
+      <main className="flex min-h-0 flex-1 flex-col">
+        <Timeline thread={thread} />
+      </main>
+      {rawOpen ? <RawLogPanel threadId={threadId} /> : null}
+    </div>
   );
 }`;

const CARGO_TEST_OUTPUT = `   Compiling vetro-proto v0.1.0 (D:\\Dev\\Projects\\vetro\\crates\\vetro-proto)
   Compiling vetro-core v0.1.0 (D:\\Dev\\Projects\\vetro\\crates\\vetro-core)
    Finished \`test\` profile [unoptimized + debuginfo] target(s) in 14.82s
     Running unittests src\\lib.rs (target\\debug\\deps\\vetro_core-4f1c2a9e.exe)

running 23 tests
test adapters::codex::tests::start_sends_cwd_only ... ok
test adapters::claude::tests::no_system_prompt_flags ... ok
test adapters::acp::tests::initialize_disables_client_fs ... ok
test sessions::tests::idle_release_skips_pending_requests ... ok
test raw::tests::rotates_at_size_cap ... ok
test links::tests::exact_link_from_session_id_line ... ok

test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.41s`;

function mainThread(): MockThread {
  const start = now - 2 * HOUR;
  const b = new Builder(start, "m");
  b.user(
    "Add a raw log panel to the thread view. It should list every line Vetro sent to or received from the agent, newest at the bottom, so people can check that nothing is injected. Use the existing `threads.raw` method.",
    [{ type: "file", path: "docs/API.md", name: "API.md" }],
  )
    .reasoning(
      "**Planning the panel**\n\nThe API already exposes `threads.raw` returning `RawEntry[]` with `direction` and `ts`. I should read the thread view first, then add a right-side panel that the header can toggle. The panel must be monospace and copyable.",
    )
    .plan([
      { content: "Read the thread view and the raw entry type", status: "completed" },
      { content: "Add a RawLogPanel component with copy support", status: "completed" },
      { content: "Wire the toggle into the thread header", status: "in_progress" },
      { content: "Run typecheck and the web build", status: "pending" },
    ])
    .tool({
      kind: "search",
      name: "shell",
      title: "Search for threads.raw",
      input: { command: ["rg", "-n", "threads.raw", "apps/web/src"] },
      content: [
        {
          type: "terminal",
          command: 'rg -n "threads.raw" apps/web/src',
          cwd: VETRO,
          output: "apps/web/src/api/protocol.ts:63:  \"threads.raw\": [ThreadRawParams, ThreadRawResult];",
          exitCode: 0,
        },
      ],
    })
    .tool({
      kind: "read",
      name: "read_file",
      title: "Read apps/web/src/components/chat/ThreadView.tsx",
      locations: [{ path: `${VETRO}\\apps\\web\\src\\components\\chat\\ThreadView.tsx`, line: 1 }],
      content: [{ type: "text", text: "42 lines" }],
    })
    .tool({
      kind: "edit",
      name: "apply_patch",
      title: "Edit apps/web/src/components/chat/ThreadView.tsx",
      locations: [{ path: `${VETRO}\\apps\\web\\src\\components\\chat\\ThreadView.tsx`, line: 1 }],
      content: [{ type: "diff", path: "apps/web/src/components/chat/ThreadView.tsx", oldText: null, newText: null, unified: RAW_LOG_PANEL_DIFF }],
    })
    .tool({
      kind: "edit",
      name: "apply_patch",
      title: "Add apps/web/src/components/RawLogPanel.tsx",
      locations: [{ path: `${VETRO}\\apps\\web\\src\\components\\RawLogPanel.tsx`, line: null }],
      content: [
        {
          type: "diff",
          path: "apps/web/src/components/RawLogPanel.tsx",
          oldText: null,
          newText:
            'export function RawLogPanel({ threadId }: { threadId: string }) {\n  const entries = useRawLog(threadId);\n  return (\n    <aside className="w-96 border-l font-mono text-xs">\n      {entries.map((entry) => (\n        <RawLine key={entry.ts} entry={entry} />\n      ))}\n    </aside>\n  );\n}',
          unified: null,
        },
      ],
    })
    .request({
      requestId: "req-1",
      callId: "call_m_5",
      title: "Run pnpm --dir apps/web typecheck?",
      detail: "The command runs outside the sandbox because it needs network access for the pnpm store.",
      body: {
        type: "permission",
        options: [
          { id: "approved", label: "Yes", kind: "allow_once" },
          { id: "approved_for_session", label: "Yes, and don't ask again for this command", kind: "allow_always" },
          { id: "denied", label: "No, tell Codex what to do differently", kind: "reject_once" },
        ],
      },
      resolution: { type: "selected", optionId: "approved" },
    })
    .tool({
      kind: "execute",
      name: "shell",
      title: "pnpm --dir apps/web typecheck",
      input: { command: ["pnpm", "--dir", "apps/web", "typecheck"], cwd: VETRO },
      content: [
        {
          type: "terminal",
          command: "pnpm --dir apps/web typecheck",
          cwd: VETRO,
          output: "> @vetro/web@0.1.0 typecheck D:\\Dev\\Projects\\vetro\\apps\\web\n> tsc -b --noEmit\n",
          exitCode: 0,
        },
      ],
    })
    .agent(
      [
        "The raw log panel is in place. It opens from the thread header and shows every wire line for the session:",
        "",
        "- **Direction** (`sent`, `received`, `stderr`, `spawn`) as a small coloured tag",
        "- **Time** with milliseconds, so ordering is easy to follow",
        "- A **Copy all** action that copies the log as JSON lines",
        "",
        "The panel reads from `threads.raw` and refreshes when new items arrive:",
        "",
        "```tsx",
        "const { entries } = await client.request(\"threads.raw\", { threadId, limit: 2000 });",
        "setEntries(entries.filter((entry) => entry.direction !== \"stderr\" || showStderr));",
        "```",
        "",
        "| Direction | Meaning |",
        "| --- | --- |",
        "| `spawn` | The exact command line and env overrides used to start the agent |",
        "| `sent` | A JSON line Vetro wrote to the agent's stdin |",
        "| `received` | A line the agent wrote to stdout |",
        "",
        "Typecheck passes. I did not run the web build yet.",
      ].join("\n"),
    )
    .end(94_000, usage(48_210, 3_480));

  b.user("Also run the core tests so we know the wire log tests still pass.")
    .reasoning("Running `cargo test -p vetro-core` covers the raw log rotation tests.")
    .tool({
      kind: "execute",
      name: "shell",
      title: "cargo test -p vetro-core",
      input: { command: ["cargo", "test", "-p", "vetro-core"], cwd: VETRO },
      content: [{ type: "terminal", command: "cargo test -p vetro-core", cwd: VETRO, output: CARGO_TEST_OUTPUT, exitCode: 0 }],
    })
    .tool({
      kind: "fetch",
      name: "web_search",
      title: "Search the web: RFC 7386 JSON merge patch null semantics",
      status: "completed",
      content: [{ type: "text", text: "RFC 7386 §2: a null value in the patch removes the member from the target." }],
    })
    .tool({
      kind: "mcp",
      name: "mcp__linear__get_issue",
      title: "linear · get_issue VET-42",
      input: { id: "VET-42" },
      output: { id: "VET-42", title: "Raw log panel", state: "In Progress" },
    })
    .notice("warning", "Codex compacted the conversation to stay within the context window.")
    .agent("All 23 `vetro-core` tests pass, including `raw::tests::rotates_at_size_cap`. Nothing else changed.")
    .end(41_000, usage(61_904, 812));

  return {
    summary: {
      id: "codex:019a7f3e-raw-log",
      agentId: "codex",
      nativeId: "019a7f3e-raw-log",
      title: "Add raw log panel to the thread view",
      preview: "All 23 vetro-core tests pass",
      cwd: VETRO,
      createdAt: start,
      updatedAt: b.lastTs,
      source: "desktop",
      origin: "Codex Desktop",
      status: "idle",
      live: true,
      resumable: true,
      parent: null,
      childCount: 0,
      model: "gpt-5.5-codex",
      archived: false,
      internal: false,
      pinned: false,
    },
    items: b.items,
    options: structuredClone(agentOptions.codex!.options),
    commands: agentOptions.codex!.commands,
    usage: {
      lastTurn: usage(61_904, 812),
      total: usage(110_114, 4_292),
      rateLimits: [
        { label: "5h", usedPercent: 34, resetsAt: now + 2 * HOUR },
        { label: "Weekly", usedPercent: 61, resetsAt: now + 3 * DAY },
      ],
    },
    raw: rawLog("codex", VETRO, b.items),
  };
}

function rawLog(agentId: string, cwd: string, items: Item[]): RawEntry[] {
  const entries: RawEntry[] = [];
  let ts = (items[0]?.ts ?? now) - 2000;
  const push = (direction: RawEntry["direction"], text: string) => {
    ts += 37;
    entries.push({ ts, direction, text });
  };
  if (agentId === "codex") {
    push("spawn", `"${agents[1]!.path}" app-server   (cwd ${cwd}; env overrides: none)`);
    push("sent", JSON.stringify({ id: 0, method: "initialize", params: { clientInfo: { name: "vetro", title: "Vetro", version: "0.1.0" } } }));
    push("received", JSON.stringify({ id: 0, result: { userAgent: "codex_cli_rs/0.98.0 (Windows 10.0.26200; x86_64)" } }));
    push("sent", JSON.stringify({ method: "initialized" }));
    push("sent", JSON.stringify({ id: 1, method: "thread/start", params: { cwd } }));
    push("received", JSON.stringify({ id: 1, result: { thread: { id: "019a7f3e-raw-log" } } }));
  } else if (agentId === "claude") {
    push("spawn", `"${agents[0]!.path}" -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --session-id 6c1d2e4f-…   (cwd ${cwd}; env overrides: none)`);
    push("sent", JSON.stringify({ type: "control_request", request_id: "init-1", request: { subtype: "initialize" } }));
    push("received", JSON.stringify({ type: "system", subtype: "init", cwd, model: "claude-opus-5-5" }));
  } else {
    push("spawn", `"${agents.find((a) => a.id === agentId)?.path}" acp --process-model in-process   (cwd ${cwd}; env overrides: none)`);
    push("sent", JSON.stringify({ jsonrpc: "2.0", id: 0, method: "initialize", params: { protocolVersion: 1, clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false } } }));
    push("received", JSON.stringify({ jsonrpc: "2.0", id: 0, result: { protocolVersion: 1, agentCapabilities: { loadSession: true } } }));
    push("sent", JSON.stringify({ jsonrpc: "2.0", id: 1, method: "session/new", params: { cwd, mcpServers: [] } }));
  }
  for (const item of items) {
    if (item.type === "user_message") {
      const text = item.content.find((block) => block.type === "text");
      ts = (item.ts ?? ts) - 40;
      const body = text?.type === "text" ? text.text : "";
      if (agentId === "codex") push("sent", JSON.stringify({ id: item.ordinal + 10, method: "turn/start", params: { threadId: "019a7f3e-raw-log", input: [{ type: "text", text: body }] } }));
      else if (agentId === "claude") push("sent", JSON.stringify({ type: "user", message: { role: "user", content: [{ type: "text", text: body }] } }));
      else push("sent", JSON.stringify({ jsonrpc: "2.0", id: item.ordinal + 10, method: "session/prompt", params: { sessionId: "s1", prompt: [{ type: "text", text: body }] } }));
    } else if (item.type === "agent_message") {
      ts = (item.ts ?? ts) - 40;
      push("received", JSON.stringify(agentId === "codex" ? { method: "item/completed", params: { item: { type: "agentMessage", text: item.text.slice(0, 80) } } } : { type: "assistant", message: { content: [{ type: "text", text: item.text.slice(0, 80) }] } }));
    } else if (item.type === "tool_call") {
      ts = (item.ts ?? ts) - 40;
      push("received", JSON.stringify({ method: "item/started", params: { item: { type: item.kind === "execute" ? "commandExecution" : "toolCall", id: item.callId } } }));
    }
  }
  push("stderr", "2026-11-23T09:14:02.118Z  INFO codex_core::codex: turn complete");
  return entries;
}

function childThreads(): { parent: MockThread; child: MockThread } {
  const start = now - 5 * HOUR;
  const childId = "codex:019a7c21-exec-review";
  const b = new Builder(start, "p");
  b.user("Review the ACP adapter for anything that could inject text into the prompt. Ask Codex for a second opinion too.")
    .reasoning("I'll read the adapter, then run `codex exec` with a focused review prompt so the second opinion is independent.")
    .tool({
      kind: "read",
      name: "Read",
      title: "Read crates/vetro-core/src/adapters/acp.rs",
      locations: [{ path: `${VETRO}\\crates\\vetro-core\\src\\adapters\\acp.rs`, line: null }],
      content: [{ type: "text", text: "412 lines" }],
    })
    .tool({
      kind: "execute",
      name: "Bash",
      callId: "toolu_01exec",
      title: 'codex exec "Review crates/vetro-core/src/adapters/acp.rs for prompt injection…"',
      input: { command: 'codex exec "Review crates/vetro-core/src/adapters/acp.rs for prompt injection. Reply with findings only."' },
      content: [
        {
          type: "terminal",
          command: 'codex exec "Review crates/vetro-core/src/adapters/acp.rs for prompt injection. Reply with findings only."',
          cwd: VETRO,
          output: "OpenAI Codex v0.98.0\n--------\nworkdir: D:\\Dev\\Projects\\vetro\nmodel: gpt-5.5-codex\nsession id: 019a7c21-exec-review\n--------\n[codex] No injection found. One note: `session/new` passes `mcpServers: []` explicitly, which is correct.\ntokens used: 18,204",
          exitCode: 0,
        },
      ],
      childThreadIds: [childId],
    })
    .agent(
      "Both reviews agree: the ACP adapter forwards prompt blocks unchanged and sends `mcpServers: []`. Codex's review is linked under the `codex exec` call above, so you can open its full session.",
    )
    .end(212_000, usage(31_002, 1_920, 200_000));

  const c = new Builder(start + 40_000, "c");
  c.user("Review crates/vetro-core/src/adapters/acp.rs for prompt injection. Reply with findings only.")
    .reasoning("Look at every place that builds an outgoing `session/prompt`, `session/new` and `initialize` message.")
    .tool({
      kind: "search",
      name: "shell",
      title: "rg -n \"session/prompt|session/new\" crates/vetro-core/src/adapters/acp.rs",
      content: [
        {
          type: "terminal",
          command: 'rg -n "session/prompt|session/new" crates/vetro-core/src/adapters/acp.rs',
          cwd: VETRO,
          output: "118:        self.call(\"session/new\", json!({ \"cwd\": cwd, \"mcpServers\": [] }))\n204:        self.call(\"session/prompt\", json!({ \"sessionId\": id, \"prompt\": blocks }))",
          exitCode: 0,
        },
      ],
    })
    .tool({
      kind: "read",
      name: "shell",
      title: "Read crates/vetro-core/src/adapters/acp.rs (lines 180-240)",
      content: [{ type: "text", text: "61 lines" }],
    })
    .agent("No injection found. One note: `session/new` passes `mcpServers: []` explicitly, which is correct.")
    .end(38_000, usage(18_204, 210));

  const parent: MockThread = {
    summary: {
      id: "claude:6c1d2e4f-acp-review",
      agentId: "claude",
      nativeId: "6c1d2e4f-acp-review",
      title: "Review the ACP adapter for injection",
      preview: "Both reviews agree",
      cwd: VETRO,
      createdAt: start,
      updatedAt: b.lastTs,
      source: "cli",
      origin: "cli",
      status: "idle",
      live: false,
      resumable: true,
      parent: null,
      childCount: 1,
      model: "claude-opus-5-5",
      archived: false,
      internal: false,
      pinned: false,
    },
    items: b.items,
    options: structuredClone(agentOptions.claude!.options),
    commands: agentOptions.claude!.commands,
    usage: { lastTurn: usage(31_002, 1_920, 200_000), total: usage(31_002, 1_920, 200_000), rateLimits: [{ label: "5h", usedPercent: 72, resetsAt: now + 50 * MIN }] },
    raw: rawLog("claude", VETRO, b.items),
  };
  const child: MockThread = {
    summary: {
      id: childId,
      agentId: "codex",
      nativeId: "019a7c21-exec-review",
      title: "Review crates/vetro-core/src/adapters/acp.rs for prompt injection",
      preview: "No injection found",
      cwd: VETRO,
      createdAt: start + 40_000,
      updatedAt: c.lastTs,
      source: "exec",
      origin: "codex_exec",
      status: "idle",
      live: false,
      resumable: true,
      parent: { threadId: parent.summary.id, callId: "toolu_01exec", confidence: "exact" },
      childCount: 0,
      model: "gpt-5.5-codex",
      archived: false,
      internal: false,
      pinned: false,
    },
    items: c.items,
    options: structuredClone(agentOptions.codex!.options),
    commands: agentOptions.codex!.commands,
    usage: { lastTurn: usage(18_204, 210), total: usage(18_204, 210), rateLimits: [] },
    raw: [],
  };
  return { parent, child };
}

function pendingThread(): MockThread {
  const start = now - 9 * MIN;
  const b = new Builder(start, "d");
  b.user("Delete the generated `target/` folders in this repo and rebuild the release binary.")
    .reasoning("Two `target` folders exist: the workspace one and `apps/desktop/src-tauri/target`. Removing them is destructive, so ask first.")
    .tool({
      kind: "execute",
      name: "run_command",
      callId: "devin-call-7",
      title: "Remove-Item -Recurse -Force target, apps/desktop/src-tauri/target",
      status: "pending",
      input: { command: "Remove-Item -Recurse -Force target, apps/desktop/src-tauri/target" },
    })
    .request({
      requestId: "req-devin-1",
      callId: "devin-call-7",
      title: "Allow Devin to run a command?",
      detail: "Remove-Item -Recurse -Force target, apps/desktop/src-tauri/target",
      body: {
        type: "permission",
        options: [
          { id: "allow-once", label: "Allow once", kind: "allow_once" },
          { id: "allow-always", label: "Always allow Remove-Item", kind: "allow_always" },
          { id: "reject-once", label: "Reject", kind: "reject_once" },
        ],
      },
      resolution: null,
    });
  return {
    summary: {
      id: "devin:lovely-quartz-4821",
      agentId: "devin",
      nativeId: "lovely-quartz-4821",
      title: "Clean target folders and rebuild release",
      preview: "Allow Devin to run a command?",
      cwd: VETRO,
      createdAt: start,
      updatedAt: b.lastTs,
      source: "vetro",
      origin: "vetro",
      status: "awaiting_input",
      live: true,
      resumable: true,
      parent: null,
      childCount: 0,
      model: "claude-opus-5-5",
      archived: false,
      internal: false,
      pinned: false,
    },
    items: b.items,
    options: structuredClone(agentOptions.devin!.options),
    commands: agentOptions.devin!.commands,
    usage: { lastTurn: null, total: usage(9_812, 402, 200_000), rateLimits: [] },
    raw: rawLog("devin", VETRO, b.items),
  };
}

function questionThread(): MockThread {
  const start = now - 26 * MIN;
  const b = new Builder(start, "q");
  b.user("Set up rate limiting for the public endpoints.")
    .reasoning("There are two reasonable designs. Ask which one the user wants before writing code.")
    .request({
      requestId: "req-q-1",
      callId: null,
      title: "A few questions before I start",
      detail: null,
      body: {
        type: "questions",
        questions: [
          {
            id: "store",
            header: "Store",
            text: "Where should counters live?",
            choices: [
              { label: "In memory", description: "Simple; resets on restart" },
              { label: "Redis", description: "Shared across instances" },
            ],
            multiSelect: false,
            allowFreeText: false,
          },
          {
            id: "routes",
            header: "Routes",
            text: "Which routes need limits?",
            choices: [
              { label: "/auth/*", description: null },
              { label: "/search", description: null },
              { label: "/upload", description: null },
            ],
            multiSelect: true,
            allowFreeText: true,
          },
        ],
      },
      resolution: null,
    });
  return {
    summary: {
      id: "claude:9a8b7c6d-rate-limit",
      agentId: "claude",
      nativeId: "9a8b7c6d-rate-limit",
      title: "Rate limiting for public endpoints",
      preview: "A few questions before I start",
      cwd: ATLAS,
      createdAt: start,
      updatedAt: b.lastTs,
      source: "vetro",
      origin: "vetro",
      status: "awaiting_input",
      live: true,
      resumable: true,
      parent: null,
      childCount: 0,
      model: "claude-sonnet-5",
      archived: false,
      internal: false,
      pinned: false,
    },
    items: b.items,
    options: structuredClone(agentOptions.claude!.options),
    commands: agentOptions.claude!.commands,
    usage: { lastTurn: null, total: usage(7_400, 380, 200_000), rateLimits: [] },
    raw: rawLog("claude", ATLAS, b.items),
  };
}

function longThread(): MockThread {
  const start = now - 3 * DAY;
  const b = new Builder(start, "l");
  const files = ["src/index/rebuild.rs", "src/index/fts.rs", "src/history/codex.rs", "src/history/claude.rs", "src/links.rs"];
  for (let turn = 1; turn <= 90; turn++) {
    const file = files[turn % files.length]!;
    b.user(`Step ${turn}: tighten error handling in \`${file}\` and keep the tests green.`)
      .reasoning(`Check how \`${file}\` reports failures today and where the errors cross a real boundary.`)
      .tool({
        kind: "read",
        name: "shell",
        title: `Read crates/vetro-core/${file}`,
        content: [{ type: "text", text: `${120 + turn} lines` }],
      })
      .tool({
        kind: "edit",
        name: "apply_patch",
        title: `Edit crates/vetro-core/${file}`,
        content: [
          {
            type: "diff",
            path: `crates/vetro-core/${file}`,
            oldText: null,
            newText: null,
            unified: `@@ -${turn},4 +${turn},4 @@\n-    let rows = stmt.query_map([], map_row).unwrap();\n+    let rows = stmt.query_map([], map_row)?;\n     for row in rows {\n         out.push(row?);\n     }`,
          },
        ],
      })
      .tool({
        kind: "execute",
        name: "shell",
        title: "cargo test -p vetro-core index",
        content: [{ type: "terminal", command: "cargo test -p vetro-core index", cwd: VETRO, output: `running ${4 + (turn % 5)} tests\ntest result: ok. ${4 + (turn % 5)} passed; 0 failed`, exitCode: 0 }],
      })
      .agent(`Replaced the \`unwrap()\` in \`${file}\` with \`?\`; the caller already maps the error into \`RpcError::internal\`. Tests pass.`)
      .end(20_000 + turn * 300, usage(20_000 + turn * 900, 300 + turn));
  }
  return {
    summary: {
      id: "codex:0199e0aa-index-hardening",
      agentId: "codex",
      nativeId: "0199e0aa-index-hardening",
      title: "Harden index rebuild error handling",
      preview: "Tests pass.",
      cwd: VETRO,
      createdAt: start,
      updatedAt: now - 2 * DAY,
      source: "cli",
      origin: "codex_cli_rs",
      status: "idle",
      live: false,
      resumable: true,
      parent: null,
      childCount: 0,
      model: "gpt-5.5-codex",
      archived: false,
      internal: false,
      pinned: false,
    },
    items: b.items,
    options: structuredClone(agentOptions.codex!.options),
    commands: agentOptions.codex!.commands,
    usage: { lastTurn: usage(101_000, 390), total: usage(101_000, 30_000), rateLimits: [] },
    raw: [],
  };
}

function simpleThread(
  summary: Omit<ThreadSummary, "status" | "live" | "parent" | "childCount" | "archived" | "createdAt" | "internal" | "pinned"> & Partial<ThreadSummary>,
  user: string,
  reply: string,
  extra?: (b: Builder) => void,
): MockThread {
  const b = new Builder(summary.updatedAt - 3 * MIN, summary.nativeId.slice(0, 4));
  b.user(user);
  extra?.(b);
  b.agent(reply).end(48_000, usage(12_000, 640));
  const opts = agentOptions[summary.agentId] ?? { options: [], commands: [] };
  return {
    summary: {
      status: "idle",
      live: false,
      parent: null,
      childCount: 0,
      archived: false,
      internal: false,
      pinned: false,
      createdAt: summary.updatedAt - 3 * MIN,
      ...summary,
    },
    items: b.items,
    options: structuredClone(opts.options),
    commands: opts.commands,
    usage: { lastTurn: usage(12_000, 640), total: usage(12_000, 640), rateLimits: [] },
    raw: [],
  };
}

export function buildThreads(): MockThread[] {
  const { parent, child } = childThreads();
  return [
    mainThread(),
    pendingThread(),
    questionThread(),
    parent,
    child,
    longThread(),
    simpleThread(
      {
        id: "codex:019a-quick-regex",
        agentId: "codex",
        nativeId: "019a-quick-regex",
        title: "Regex for ISO dates",
        preview: "Use a lookahead",
        cwd: QUICK,
        updatedAt: now - 5 * HOUR,
        source: "desktop",
        origin: "Codex Desktop",
        resumable: true,
        model: "gpt-5.5-codex",
      },
      "Write a regex that matches ISO dates.",
      "Use a four-digit year, two-digit month and day, then add a lookahead for a time.",
    ),
    simpleThread(
      {
        id: "antigravity:conv-3f9e-landing",
        agentId: "antigravity",
        nativeId: "conv-3f9e-landing",
        title: "Landing page hero copy",
        preview: "Here are three options",
        cwd: NOTES,
        updatedAt: now - 3 * HOUR,
        source: "desktop",
        origin: "Antigravity",
        resumable: false,
        model: "gemini-3-pro",
      },
      "Write three options for the hero headline of the field notes site.",
      "Here are three options:\n\n1. **Notes from the field, kept simple.**\n2. **Everything you saw, in one place.**\n3. **Write it down before you forget it.**",
    ),
    simpleThread(
      {
        id: "antigravity:sess-a17c",
        agentId: "antigravity",
        nativeId: "sess-a17c",
        title: "Explain the markdown pipeline",
        preview: "The site renders markdown in two passes",
        cwd: NOTES,
        updatedAt: now - 1 * DAY - 2 * HOUR,
        source: "cli",
        origin: "agy",
        resumable: true,
        model: "gemini-3-flash",
      },
      "How does the markdown pipeline work in this repo?",
      "The site renders markdown in two passes: `remark` parses and transforms the AST, then `rehype` turns it into HTML with syntax highlighting.",
    ),
    simpleThread(
      {
        id: "claude:2b3c4d5e-pagination",
        agentId: "claude",
        nativeId: "2b3c4d5e-pagination",
        title: "Cursor pagination for /search",
        preview: "Switched to keyset pagination",
        cwd: ATLAS,
        updatedAt: now - 5 * HOUR,
        source: "desktop",
        origin: "claude-desktop",
        resumable: true,
        model: "claude-opus-5-5",
      },
      "Replace offset pagination on /search with cursor pagination.",
      "Switched to keyset pagination on `(created_at, id)`. The cursor is an opaque base64 string; old `?page=` links return 400 with a hint.",
    ),
    simpleThread(
      {
        id: "codex:0199f1-migrations",
        agentId: "codex",
        nativeId: "0199f1-migrations",
        title: "Squash old migrations",
        preview: "Squashed 41 migrations into one baseline",
        cwd: ATLAS,
        updatedAt: now - 2 * DAY,
        source: "ide",
        origin: "codex_vscode",
        resumable: true,
        model: "gpt-5.5",
      },
      "Squash the migrations older than v2.0 into one baseline.",
      "Squashed 41 migrations into `0001_baseline.sql`. A fresh database built from the baseline matches the old chain table by table.",
    ),
    simpleThread(
      {
        id: "devin:brisk-otter-1180",
        agentId: "devin",
        nativeId: "brisk-otter-1180",
        title: "Tauri window state on Windows",
        preview: "The window position is restored",
        cwd: SPIKE,
        updatedAt: now - 6 * DAY,
        source: "cli",
        origin: "devin",
        resumable: true,
        model: "swe-2",
      },
      "Make the window remember its size and position on Windows.",
      "The window position is restored with `tauri-plugin-window-state`. Multi-monitor setups fall back to the primary display when the saved monitor is gone.",
    ),
    simpleThread(
      {
        id: "codex:0199c2-ci",
        agentId: "codex",
        nativeId: "0199c2-ci",
        title: "Nightly CI for Windows",
        preview: "Added a windows-latest job",
        cwd: VETRO,
        updatedAt: now - 9 * DAY,
        source: "cloud",
        origin: "codex_cloud",
        resumable: false,
        model: "gpt-5.5-codex",
        archived: false,
      },
      "Add a nightly CI job on Windows.",
      "Added a `windows-latest` job that runs `cargo test --workspace` and the web build every night at 03:00 UTC.",
    ),
    simpleThread(
      {
        id: "codex:subAgent04d7-index-sweep",
        agentId: "codex",
        nativeId: "subAgent04d7-index-sweep",
        title: "subAgent · index sweep",
        preview: "Removed 0 stale entries",
        cwd: VETRO,
        updatedAt: now - 40 * MIN,
        source: "exec",
        origin: "codex_exec",
        resumable: false,
        model: "gpt-5.5-codex",
        internal: true,
      },
      "Sweep the session index for stale entries and report the count.",
      "Removed 0 stale entries; the index was already clean.",
    ),
    simpleThread(
      {
        id: "claude:scratch-9e2f",
        agentId: "claude",
        nativeId: "scratch-9e2f",
        title: "Scratch: reconnecting ws client",
        preview: "Backoff and replay work",
        cwd: SCRATCH,
        updatedAt: now - 12 * DAY,
        source: "cli",
        origin: "cli",
        resumable: true,
        model: "claude-sonnet-5",
      },
      "Wire up a reconnecting websocket client in this scratch folder.",
      "The client reconnects with exponential backoff and replays the last subscription.",
    ),
    simpleThread(
      {
        id: "claude:old-archived",
        agentId: "claude",
        nativeId: "old-archived",
        title: "Old spike: Electron shell",
        preview: "Dropped in favour of Tauri",
        cwd: VETRO,
        updatedAt: now - 20 * DAY,
        source: "cli",
        origin: "cli",
        resumable: true,
        model: "claude-sonnet-5",
        archived: true,
      },
      "Try an Electron shell.",
      "Dropped in favour of Tauri: the Electron build was 180 MB.",
    ),
  ];
}

export const settings: Settings = {
  agents: {
    codex: { enabled: true, binaryPath: null, extraArgs: [], env: {} },
    claude: { enabled: true, binaryPath: null, extraArgs: [], env: {} },
  },
  customAcpAgents: [{ id: "goose", name: "Goose", command: "goose", args: ["acp"], env: {} }],
  idleReleaseMinutes: 15,
  closeToTray: true,
  port: 47600,
  externalEditor: null,
  notificationsEnabled: true,
};

export const remote: RemoteStatus = {
  enabled: true,
  port: 47600,
  tailscale: { installed: true, running: true, dnsName: "chris-desk.tail4b21.ts.net", serveUrl: "https://chris-desk.tail4b21.ts.net" },
  devices: [
    { id: "dev-iphone", name: "Alex's iPhone", createdAt: now - 12 * DAY, lastSeenAt: now - 40 * MIN, current: false },
    { id: "dev-ipad", name: "iPad Air", createdAt: now - 30 * DAY, lastSeenAt: now - 4 * DAY, current: false },
  ],
};

export const fileTree: Record<string, string[]> = {
  "": ["C:\\", "D:\\"],
  "C:\\": ["Program Files", "Users", "Windows"],
  "C:\\Users": ["Alex", "Public"],
  [HOME]: ["Desktop", "Documents", "Downloads", "scratch", ".vetro"],
  [`${HOME}\\scratch`]: ["tauri-spike-old", "notes.txt"],
  "D:\\": ["Dev", "Software", "Media"],
  "D:\\Dev": ["Projects", "Sandbox"],
  "D:\\Dev\\Projects": ["vetro", "atlas-api", "field-notes", "ledger", "pixel-forge", "README.md"],
  "D:\\Dev\\Projects\\ledger": ["src", "Cargo.toml", "README.md"],
  "D:\\Dev\\Projects\\pixel-forge": ["src", "package.json"],
  "D:\\Dev\\Sandbox": ["playground"],
};

export const projectFiles = [
  "AGENTS.md",
  "Cargo.toml",
  "README.md",
  "docs/API.md",
  "docs/ARCHITECTURE.md",
  "crates/vetro-proto/src/lib.rs",
  "crates/vetro-core/src/lib.rs",
  "crates/vetro-core/src/server.rs",
  "crates/vetro-core/src/sessions.rs",
  "crates/vetro-core/src/adapters/acp.rs",
  "crates/vetro-core/src/adapters/claude.rs",
  "crates/vetro-core/src/adapters/codex.rs",
  "crates/vetro-core/src/history/codex.rs",
  "crates/vetro-core/src/index/rebuild.rs",
  "crates/vetro-cli/src/main.rs",
  "apps/web/package.json",
  "apps/web/src/main.tsx",
  "apps/web/src/App.tsx",
  "apps/web/src/components/chat/ThreadView.tsx",
  "apps/web/src/components/chat/Composer.tsx",
  "apps/web/src/components/sidebar/Sidebar.tsx",
];
