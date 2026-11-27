# Verre: instructions for coding agents

Verre is a lightweight, clean, unified entry point for coding agents that are already installed on the user's machine: Claude Code, Codex, Antigravity and Devin. It starts the official, unmodified agent binaries, shows their sessions in one interface modelled on T3 Code, and stays out of the way. It is a thin client. It is never an agent harness.

Read `docs/ARCHITECTURE.md` before changing the core, `docs/API.md` before touching the UI protocol, and `docs/DESIGN.md` before adding features or changing the UI.

## Product rules (never break these)

1. **Zero injection.** What the user typed is what the agent receives. Never add system prompts, developer instructions, skills, MCP servers, tools, wrapper text, or replayed history. Model, mode, and thinking-level choices are passed only through the agent's own switches, and only when the user changed them. (Client-side input helpers like voice transcription or draft restore fill the composer visibly before sending.)
2. **Zero footprint.** Verre writes its own state only under `~/.verre` (`VERRE_HOME` overrides it). Never create, modify, or delete anything under `~/.claude`, `~/.codex`, `~/.gemini`, `~/.agents`, Devin's config directories, or other tools' stores; open their SQLite files read-only and never checkpoint their WAL. Never silently write hidden files or config into a project workspace — only explicit, user-triggered standard workspace actions (such as creating a folder) may touch the workspace, leaving no Verre residue. (The agents themselves keep writing their own sessions and logs; that is native behaviour.)
3. **Native runtimes only.** Launch the official agent binaries found on the machine. Never download an agent binary. Never read, copy, forward, or store credentials. Sign-in always happens inside the agent's own flow. (Optional client-side extras such as offline voice models live under `~/.verre`, stay zero-download by default, and are installed or removed only when requested by the user.)
4. **No orphans.** Every managed child process dies when Verre exits (Windows Job Object; Unix process group and descendant watchdog). While running, warn and ask for confirmation before releasing a session or quitting if active background work is detected rather than silently killing it.
5. **Light — in runtime and scope.** One Rust process, system WebView; no Node, Electron, or bundled browser at runtime. Idle agent processes are released and resumed on demand. Stay light in product scope too: Verre is a thin client for conversation, approvals, history, read-only diff inspection, and private remote access — never a built-in IDE, code editor, terminal emulator, or multi-agent workflow orchestrator. Hand editing and terminal work off to external tools.
6. **Capability-driven, not privileged.** The UI skeleton, slot order, and interaction rules treat every agent equally — no agent gets artificial restrictions or proprietary favoritism. When an agent's native protocol exposes a useful capability (rate limits, mid-turn steering, fork, multi-dimensional permissions), surface it through unified capability slots rather than hiding native value or building bespoke one-off UIs.
7. **Progressive disclosure.** The primary interface surfaces only user intent, items needing human action, and final answers. Intermediate reasoning, tool logs, code diffs, linked child sessions, internal or automated sessions, and raw protocol traffic stay collapsed or inside on-demand drawers by default — nothing is hidden from inspection, and nothing competes with the conversation.

## Design & anti-bloat discipline

Every feature or UI change must protect the whole product from incremental bloat and drift. See `docs/DESIGN.md` for details.

- **Decision hierarchy:** (1) the 7 product rules above, then (2) T3 Code as the concrete UI/UX reference (read-only checkout at `.orchestra/ref/t3code` when available), then (3) the specific feature request.
- **Check T3 Code before designing UI:** For layout, button placement, controls, interaction hierarchy, and settings grouping, inspect `.orchestra/ref/t3code` first and follow its structure by default. Never copy anything from T3 Code that breaks Verre's product rules (prompt/MCP injection, heavy Electron/Node runtime, agent-specific privilege, bloated harness-only tabs) and never copy its name, trademark, or logos.
- **No dumping into existing buckets:** Never lazily append controls into the composer bar or the "General" settings section. Prefer sensible defaults over new toggles; split a dedicated settings section once a sub-domain has more than ~2 related settings or a section feels crowded.
- **Guard the whole, not just the diff:** Before implementing, check whether the change makes the surrounding surface awkward or inconsistent across agents, and tidy the affected structure instead of stacking patches. If a user request risks bloating or drifting Verre away from its philosophy, speak up plainly and propose a cleaner alternative before coding.

## Layout

- `crates/verre-proto`: wire contract (Rust types, TypeScript bindings generated by `cargo test -p verre-proto`). Owned by the orchestrator: do not change it without saying so in your report.
- `crates/verre-core`: agent adapters, session manager, history readers, linking, index, HTTP/WebSocket server.
- `crates/verre-cli`: the `verre` binary (`verre serve`, `verre probe`, ...).
- `apps/web`: React UI (Vite, Tailwind 4, Base UI). Built output is embedded in the core.
- `apps/desktop`: Tauri 2 shell.
- `docs/`: architecture, API, protocol notes per agent.

## Commands

- Rust: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Web: `pnpm --dir apps/web install`, `pnpm --dir apps/web typecheck`, `pnpm --dir apps/web build`, `pnpm --dir apps/web dev`.
- On this Windows machine cargo lives in `~/.cargo/bin`; add it to `PATH` if the shell does not have it.

## Conventions

- English in code, comments, docs and commit messages. Chinese only in `README.zh-CN.md` and UI translations.
- Promptly clean up unused build artifacts when doing so will not disrupt development. Keep active builds and binaries in use.
- No comments unless the code cannot show why.
- Handle errors at real boundaries; do not wrap every line.
- New dependencies: add with `cargo add` / `pnpm add`, prefer versions published more than 7 days ago, keep the tree small. The core must stay light.
- Commits: small, imperative subject line, no AI attribution. Never push, never create remotes.
- Tests that need a live agent are `#[ignore]` and are run on purpose, never in the default test run. Default tests use the fake agents in `crates/verre-core/tests/fixtures`.

## Live agents on the development machine

- Binaries are found the way the app finds them (`verre agents` shows the paths). Override with `VERRE_ACP_DEVIN` and the settings file when a test needs a specific binary.
- Live tests spend the owner's quota. Keep prompts tiny (for example: reply with one word), use the cheapest model, and do not loop.
- Subscriptions have small quotas: at most a handful of short live prompts per task.
- `devin acp` must be started with `--process-model in-process` in tests so no daemon is left behind. It needs the ACP `authenticate` call (`devin-browser`) in every new process; that opens a browser tab and completes by itself.
- Never run agents with permission-bypass flags against anything but a scratch directory under `.orchestra/`.

## When you are called non-interactively by the orchestrator

- Do not read, update, commit or push `~/ai-memory`; the orchestrator keeps the records.
- Do not call other agent CLIs as helpers. Starting them to test Verre's adapters is the job; delegating your work to them is not.
- Stay inside the task. If it cannot be done as written, stop and say what is missing.
- Finish with a short report: what changed, what you ran and its result, what you could not verify, open questions.
