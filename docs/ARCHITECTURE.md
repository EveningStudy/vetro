# Vetro architecture

## Shape

```
 desktop window (Tauri, system WebView)      browser / phone (via tailscale serve)
              \                                   /
               \        HTTP + one WebSocket     /
                +----------- vetro-core --------+        one Rust process
                |  server      (axum, embedded UI, auth, pairing)
                |  sessions    (live threads, event log, subscriptions, idle release)
                |  adapters    claude | codex | acp (antigravity, devin, custom)
                |  history     read-only readers of each agent's own store
                |  links       parent/child sessions across agents
                |  index       ~/.vetro/index.db (SQLite + FTS5)
                +-------------------------------+
                     | stdio (JSON lines), one child process tree, job object / process group
        claude -p (stream-json)   codex app-server   agy acp   devin acp
```

The desktop app and `vetro serve` are the same core. The Tauri shell only creates a window that loads the core's local URL, a tray icon and single-instance handling. The UI is identical in the window and in a remote browser.

## Data directory

Everything Vetro writes is under `~/.vetro` (`VETRO_HOME` overrides):

- `config.json`: `Settings` from `vetro-proto`.
- `state.json`: local token, paired devices, per-thread overlay (title override, archived), per-project overlay (name, pinned, hidden), cached agent options.
- `index.db`: thread index and full-text search. Disposable; rebuilt from the agents' stores.
- `raw/<thread-id>.jsonl`: wire log of each session attached in Vetro (rotated, size-capped).
- `logs/`: Vetro's own logs.

## Agent adapters

Adapters implement `vetro_agent_api::Agent` and emit `SessionEvent`s. The core assigns item ordinals and produces the UI's `vetro_proto::ThreadEvent`s. The shared process transport records the spawn and stdio lines for the UI's raw view. Sessions multiplexed on the same process share its log.

Detection: a binary is looked up in `Settings.agents[id].binaryPath`, then `PATH`, then well-known install locations (npm global shims are resolved to the real executable; the Claude desktop app's bundled `claude` and Devin Desktop's bundled `devin` are fallbacks). Detection never installs or downloads anything.

### Codex: `codex app-server`

The native protocol of the Codex desktop app and IDE extension. One long-lived process serves every Codex thread.

- `initialize` with `clientInfo { name: "vetro", title: "Vetro", version }`, then `initialized`.
- New thread: `thread/start` with `cwd` only. Model, reasoning effort, approval policy and sandbox are sent only when the user changed them. Never send `baseInstructions`, `developerInstructions`, dynamic tools or config overrides.
- Turn: `turn/start`; while running, a new message uses `turn/steer`; stop is `turn/interrupt`.
- Server requests (`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`, `item/tool/requestUserInput`, `mcpServer/elicitation/request`) become `InteractionRequest` items.
- History: `thread/list` (all source kinds, paged) and `thread/read` with turns; continue with `thread/resume`.
- Options: `model/list` (models and their reasoning efforts), the collaboration/approval presets the server reports. Account: `account/read`, `account/login/*`, `account/rateLimits/read`.
- Everything in `~/.codex/config.toml` keeps working because Codex itself reads it: ChatGPT login, API keys, custom providers and gateways, hooks, MCP servers, skills. Vetro has no provider logic.
- Generate the protocol types for the installed version with `codex app-server generate-json-schema` / `generate-ts` when in doubt; tolerate unknown fields and methods.

### Claude Code: `claude -p` with stream-json in both directions

This is the protocol the official Claude Agent SDK uses under the hood. One process per live thread.

- Start: `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages` plus `--session-id <uuid>` (new) or `--resume <id>` (existing), in the thread's cwd. Permission prompts go to the host over the stdio control channel (verify the exact flag against the installed CLI and the SDK's own spawn code).
- Never pass `--system-prompt`, `--append-system-prompt`, `--mcp-config`, `--settings`, `--setting-sources`, `--tools`, `--allowedTools`, `--bare` or `--safe-mode`. With none of them the CLI uses its own Claude Code system prompt and loads user, project and local settings, so hooks, `CLAUDE.md`, skills, plugins and MCP servers behave as in the terminal.
- Control protocol: `initialize`, `can_use_tool` (permission requests, including `AskUserQuestion`), `interrupt`, `set_permission_mode`, `set_model`. Reference: the `@anthropic-ai/claude-agent-sdk` package (types in `sdk.d.ts`) and the open-source Python SDK.
- History: `~/.claude/projects/<encoded cwd>/<session>.jsonl`, read-only. Sidechain (sub-agent) records are shown under their `Task` tool call.

### ACP agents: Antigravity (`agy acp`), Devin (`devin acp`), custom commands

One generic ACP v1 client (`agent-client-protocol` crate or a small hand-written JSON-RPC layer; pick whichever is lighter and more tolerant). One process per agent serves all of that agent's sessions.

- `initialize` with `clientCapabilities { fs: { readTextFile: false, writeTextFile: false }, terminal: false }` so the agent uses its own tools and Vetro adds nothing.
- `session/new` with `cwd` and `mcpServers: []`. Prompts are forwarded as typed: text, images, resource links for mentioned files.
- `session/update` notifications map one-to-one to items; `session/request_permission` and elicitation become `InteractionRequest`s; `session/cancel` stops a turn.
- Models, modes and thinking levels come from the agent's `configOptions` / `modes` and are changed with `session/set_config_option` / `session/set_mode`.
- History: `session/list` where advertised, `session/load` to read and continue.
- Auth: when the agent answers `auth_required`, the UI offers the agent's own `authMethods`; `authenticate` runs the agent's flow. Vetro never sees a credential.
- Antigravity is launched exactly as Google documents for ACP clients: the unmodified `agy` binary with the single argument `acp`, signed in with its own cached login (or any other method the agent offers, such as a Gemini API key from the environment). See `docs/agents/antigravity.md` for the policy background.
- Devin is launched as `devin acp --process-model in-process`.

## Sessions

- A thread id is `<agentId>:<nativeSessionId>`. Threads exist whether or not Vetro started them; attaching a live native session to one makes it `live`.
- Each open thread keeps its items in memory and an event log with a per-thread `seq`. Clients open a thread, get the newest page of items plus the current `seq`, then receive `thread.event`s. After a reconnect the client opens the thread again and replaces its copy.
- Sending to a thread that is not live resumes the native session first (`thread/resume`, `--resume`, `session/load` or `session/resume`). If the agent cannot resume it, the thread is read-only (`resumable: false`).
- Agent processes with no running turn and no open request are stopped after `idleReleaseMinutes`; the next message resumes the session. A thread waiting on a permission request is never released.
- All children are in one Windows Job Object (`KILL_ON_JOB_CLOSE`) or Unix process group and are killed when the core exits, also on panic or kill of the parent.

## History (read-only)

Four readers produce `ThreadSummary` rows for the index and items on demand:

| Agent | Listing | Items | Continue |
|---|---|---|---|
| Codex | `thread/list` (every source kind) | `thread/read` with turns | `thread/resume` |
| Claude Code | scan `~/.claude/projects/**/*.jsonl` | parse the JSONL | `--resume <id>` |
| Antigravity (CLI/ACP store) | ACP `session/list` | ACP `session/load` | same session |
| Antigravity (desktop app store) | `~/.gemini/antigravity/conversation_summaries.db` | `conversations/<id>.db` (`steps` protobuf, decoded best-effort; text from the FTS tables) | read-only unless ACP can load it |
| Devin | ACP `session/list` | ACP `session/load` | same session |

Rules: open foreign SQLite with `mode=ro` (and `immutable=1` when there is no live writer to respect), never create `-wal`/`-shm` files next to them, never hold locks. The index stores summaries and searchable text only and can be deleted at any time. A file watcher keeps threads that are being written by other apps fresh; watching must not keep files locked on Windows.

`ThreadSource` comes from the agent's own records: Codex `originator`/`source`, Claude `entrypoint`, the Antigravity store the session lives in, Devin's session metadata.

## Links between agents

When an agent runs another agent's CLI (`codex exec`, `claude -p`, `agy -p`, `devin -p`), the child session is stored by the child agent and is invisible in the parent's app. The linker finds the parent tool call for every `Exec`-sourced session:

1. **Exact**: the child's session id appears in the parent's tool output (Codex prints `session id: <id>`; `claude -p --output-format json` returns `session_id`; `agy -p --output-format json` returns `conversation_id`), or the agent's own store names the parent.
2. **Prompt**: the child's first user message is contained in the parent's command line or in a file that command read from stdin.
3. **Heuristic**: an execute tool call that invokes that CLI, in the same working directory, whose time window contains the child's start.

A link sets `ThreadSummary.parent` on the child and adds the child to `ToolCall.childThreadIds` on the parent's tool call. Links are recomputed incrementally as either side changes, so a running child shows up under the running tool call and streams as its file grows.

## Server and remote access

- The server binds `127.0.0.1` only. Remote devices reach it through `tailscale serve` (or any reverse proxy the user runs); Vetro never listens on other interfaces and never opens firewall ports.
- Every request needs a session cookie. The desktop window gets it by exchanging the local token from `state.json`; a new device gets it by entering a one-time pairing code shown on the desktop. Check `Origin` and `Host` on every request and on the WebSocket upgrade.
- HTTP endpoints: `GET /api/health`, `POST /api/auth/local`, `POST /api/auth/pair`, `GET /api/ws`, and the static UI with SPA fallback. Everything else goes over the WebSocket (see `docs/API.md`).
- The UI is embedded in the binary from `apps/web/dist`. In development `VETRO_WEB_DIR` serves it from disk, and the Vite dev server proxies `/api` to the core.

## Verifiable cleanliness

1. **Raw view.** `threads.raw` returns the wire log, including the command line and the names of environment overrides used to start the agent. Environment override values are masked.
2. **Footprint test.** An integration test runs full sessions against fake agents with `HOME`/`USERPROFILE` pointed at a temp directory, snapshots the tree before and after, and fails on any change outside `.vetro` that the fake agent did not make itself.
3. **No-injection test.** For every adapter, a test sends a prompt through a fake agent and asserts that the user text arrives byte-for-byte and that no system prompt, instruction, tool or MCP field is present in any outgoing message.
4. **Orphan test.** Start agents, kill the core hard, assert no child survives.

## Milestones

- **M0** core skeleton, three adapters, `vetro probe <agent>` (new session, one prompt, streaming, one permission round-trip, resume by id) with wire logs.
- **M1** server + UI + desktop shell on Windows: new and continued threads for all four agents.
- **M2** history for all four agents, filters, full-text search.
- **M3** links between agents, live child sessions.
- **M4** remote access: pairing, `tailscale serve`, phone layout, reconnect.
- **M5** macOS and Linux builds, installers, `vetro serve`, docs.
