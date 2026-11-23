# Antigravity conversation store (read-only)

Crate: `crates/vetro-store-antigravity`. It lists and reads the conversations that Antigravity keeps on disk, so sessions that were not started through Vetro show up with their full history. The format has no published schema; everything below was worked out from real stores (structure only) and may change with Antigravity updates. Unknown data never causes an error: it is skipped or shown as a generic item.

```rust
let store = Store::open(home.join(".gemini/antigravity"));      // never creates anything
let sessions: Vec<SessionRecord> = store.list()?;               // newest first
let transcript: Transcript = store.read(&sessions[0].native_id)?;
```

Measured on 2026-11-23 against 79 desktop and 5 CLI conversations (12.7k steps, the largest file 164 MB).

## Stores

| Root | Written by | `ThreadSource` |
|---|---|---|
| `~/.gemini/antigravity` | the desktop app | `Desktop` |
| `~/.gemini/antigravity-cli` | `agy` (TUI, `-p`, `acp`) | `Exec` for `agy -p` runs, otherwise `Cli` |

Both roots have the same layout:

- `conversation_summaries.db`: one row per conversation **of both stores** (each app mirrors the other's list). A store only lists conversations whose file is in its own `conversations/` directory; the rest belong to the sibling store.
- `conversations/<id>.db`: one SQLite file per conversation.
- `log/cli-*.log`, `log/acp-server-*.log`: the CLI's own logs. They are the only place that says whether a CLI conversation came from `agy -p`: a log containing `printmode.go` / `Print mode:` and `Created conversation <id>` marks `<id>` as `Exec` (origin `... print`); an `acp-server-*` log marks it as an ACP session (origin `... acp`). Nothing in the databases distinguishes the modes.

All files are in WAL mode. The app opens them briefly and deletes `-wal`/`-shm` on close, so most of the time there is no `-wal`.

## Read-only access

`sqlite::open_readonly` never writes, creates or deletes a file:

- SQLite's `mode=ro` still creates `-wal` and `-shm` next to a WAL database when they are missing, and on Windows it opens `-shm` with `OPEN_ALWAYS`. Vetro therefore registers a VFS shim (`vetro-readonly`) over the platform VFS. For every named file it refuses to open a file that does not exist, refuses a `-wal` whose `-shm` is missing, strips `READWRITE`/`CREATE` and forces `READONLY`; `xDelete` always fails.
- `-wal` present: open `mode=ro&readonly_shm=1`. The reader sees committed frames that are not checkpointed yet. `-shm` is opened read-only, so the app's wal-index is never modified. The connection holds a shared lock only while it is open (a few ms to a few hundred ms); WAL writers are not blocked.
- No `-wal` (or the shim refused one, for example an orphaned `-wal` without `-shm`): open `immutable=1`, which takes no locks and ignores WAL.
- `query_only` and `temp_store=memory` are set, connections are closed right after use, and steps are streamed row by row (only the decoded items are kept).

## `conversation_summaries.db`

Table `conversation_summaries`, columns used: `conversation_id`, `title`, `preview`, `last_modified_time` (text, `2026-11-01 01:02:44.8475682+00:00`; the zero time `0001-01-01` means unset), `workspace_uris` (JSON array of `file:///d%3A/...` URIs, often empty), `source` (`CORTEX_TRAJECTORY_SOURCE_CLI` or empty), `app_data_dir` (`antigravity` / `antigravity-cli`, not reliable for the file location), `parent_conversation_id`, `raw_summary`. Table `conversation_parents(conversation_id, parent_id)` takes precedence for the parent. The FTS5 table `conversations_fts` is not used.

`raw_summary` (protobuf): `3` last modified (Timestamp), `7` created (Timestamp), `9.1` workspace URI, `17` trajectory info (`2` created, `8` sub-agent definition: `8.1` agent type, `8.2` role name).

`SessionRecord` mapping:

| Field | Source |
|---|---|
| `title` | `title`, else sub-agent role `raw_summary.17.8.2`, else the first line of the preview, else `Untitled conversation` |
| `preview` | `preview`, else the first user input of the conversation file |
| `cwd` | first `workspace_uris` entry, else `raw_summary.9.1`, else `trajectory_metadata_blob.1.1` |
| `created_at` | `raw_summary.7`, else `raw_summary.17.2`, else `trajectory_metadata_blob.2` |
| `updated_at` | `last_modified_time`, else the file mtime (or the `-wal` mtime when it has data) |
| `origin` | `source` if set, else `app_data_dir`, else the root directory name; plus ` print` / ` acp` from the CLI logs |
| `parent_native_id` | `conversation_parents`, else `parent_conversation_id` |
| `resumable` | always `false` |
| `model` | `None` (the model is stored as an internal enum number) |

Conversation files without a summary row are listed from the file itself (`trajectory_metadata_blob` and the first user input).

## `conversations/<id>.db`

Tables: `steps(idx, step_type, status, has_subtrajectory, metadata, error_details, permissions, task_details, render_info, step_payload, step_format)`, `trajectory_meta(trajectory_id, cascade_id, trajectory_type, source)` (source `1` desktop, `17` CLI), `trajectory_metadata_blob` (`id = 'main'`: `1.1` workspace URI, `2` created, `7` workspace URI, `18` project id), `gen_metadata` (model request context, by far the largest table, not read), `executor_metadata`, `parent_references`, `battle_mode_infos`. `step_format` was always `0` and `permissions` was empty. `render_info` repeats the tool display data (`140.2.3`, below) for about half of the tool steps, and `task_details` repeats payload field `148` (background task state of `run_command` and `schedule`); both are covered by `step_payload`.

Only `idx`, `step_type`, `status`, `step_payload` and `error_details` are read.

### Step envelope (`step_payload`)

| Field | Meaning |
|---|---|
| 1 | step type (same as the column) |
| 4 | status (same as the column) |
| 5 | step metadata (identical to the `metadata` column) |
| one other field | the body; its number depends on the type (below) |

Step metadata: `1` created (Timestamp, used as the item time), `3` origin of the step (`2` model, `4` user, `5` system), `4` tool call (`1` call id, `2` tool name, `3` arguments JSON), `6`/`7`/`8`/`32` further timestamps, `9` model usage, `11` model enum, `12` execution id, `20` position (`2` step index, `4` conversation id), `26` status history, `35` step id.

Status: `3` done, `5` done and compacted (the app stripped arguments and raw results from old steps and kept the display data), `6` cancelled, `7` error, `0`-`2`/`4` not observed. `error_details`: `2` message, `3` detail.

### Step types

| Type | Body field | Seen | Mapped to |
|---|---|---|---|
| 14 user input | 19 | 178 | `UserMessage`, starts a turn |
| 15 planner response | 20 | 3995 | `Reasoning`, `AgentMessage`, and a placeholder `ToolCall` per call |
| 17 ephemeral system message | 24 | 15 | `Notice` (info) |
| 23 context compaction | 30 | 14 | `Notice` (info) "Context compacted", summary in `detail` |
| 101 system / inter-agent message | 114 | 322 | `UserMessage` when it is the opening prompt of a sub-agent, otherwise `Notice` (info) |
| 132 tool execution | 140 | 8239 | `ToolCall` (replaces the planner placeholder with the same call id, keeping its position) |
| anything else | first non-envelope field | 0 | `ToolCall` kind `Other`, name `antigravity_step_<type>`, title from the first readable string |

Error details on a non-tool step add a `Notice` (error).

**14, user input** (`19`): `2` the text as typed; `3` repeated parts (`1` text; used when `2` is missing); `9` repeated attached images (`1` MIME type, `4` id, `5` local path, `6` bytes; up to 8 MiB inline, otherwise by path); `12`, `13`, `18` run configuration (not read).

**15, planner response** (`20`): `1` response text (markdown), `8` copy of `1`, `3` thinking text, `6` message id, `7` repeated tool calls (`1` call id, `2` tool name, `3` arguments JSON), `11` thinking duration, `12` unknown enum, `14` signature.

**17, ephemeral** (`24`): `3.2` text.

**23, compaction** (`30`): `5` summary text; `14`, `20`, `26`, `30` carried-over state (not read).

**101, system message** (`114`): `1` formatted text with a header, `2` display (`1` title, `10.1` body), `3` kind (`agent_message`, `task_notification`, `plan_mode_handoff`, `plan_mode_handback`, `plan_mode_review`, `side_question_boundary`, or empty for system and plugin messages), `4` routing (`2` recipient conversation, `3` sender conversation, `4` body).

**132, tool execution** (`140`):

- `1` repeated arguments (`1` name, `2` value as text). Used when the call in the metadata has no JSON arguments.
- `2` result:
  - `1` the text returned to the model, prefixed with `Created At:` / `Completed At:` lines (stripped);
  - `2` repeated key/value annotations;
  - `3` display data: `5` verb (`Ran`, `Edited`, `Analyzed`, ...), `6` icon, `7` category, `9` repeated chips (`1` label; `2` file: `1` URI, `2`/`3` lines; `4` diff stats: `1` URI, `2.1` added, `2.2` removed; `5` sub-agent: `1` conversation id, `2` role, `3` type), `10` body (`1` markdown; `2` image; `3` terminal: `1` command, `2` cwd, `3` exit code, `4` output; `4` rows);
  - `4` image (`1` MIME type, `2` bytes);
  - `6` typed result as `google.protobuf.Any` (`1` type URL, `2` message);
  - `7` artifact URI.

Typed results that are decoded (type URL suffix):

| Type | Fields |
|---|---|
| `RunCommand` | `1` exit code (absent when 0), `5` shell, `6` command line, `7` cwd |
| `FileEdit` (`edit_file`, `write_file`) | `2` path, `3.3` repeated lines (`1` text, `2` op: `1` inserted, `2` deleted, `3` unchanged; the whole file), `4` stats (`1` added, `2` removed) |
| `ReadFile` | `3` path, `4` first line, `5` last line, `2` bytes, `6` characters |
| `SearchWeb` | `1` query, `3` repeated sources (`1` URL, `3` URL, `4` site), `4` answer |
| `McpTool` | `1` server, `2` call (`1` id, `2` tool, `3` arguments), `3` result |
| `InvokeSubagent` | `1` spec (`1` type, `2` role, `3` prompt), `2` child (`1` conversation id, `2` brain URI, `3` workspace URI) |

`SendMessage`, `ManageTask`, `ManageSubagents`, `Schedule`, `GenerateImage`, `DefineSubagent` and `WebviewBrowser` are shown through the display data and result text only.

### Tool calls

Tool names seen: `run_command`, `read_file`, `view_file`, `edit_file`, `write_file`, `search_web`, `search_marketplace`, `webview_browser`, `call_mcp_tool`, `invoke_subagent`, `define_subagent`, `manage_subagents`, `send_message`, `manage_task`, `schedule`, `generate_image`, `enter_plan_mode`, `exit_plan_mode`, `update_focus`, `list_plugin_accounts`, `swarm`.

| Tool | `ToolKind` | Content |
|---|---|---|
| `run_command` | `Execute` | `Terminal` (command, cwd, output, exit code) from the display terminal, else the typed result and result text |
| `edit_file`, `write_file` | `Edit` | `Diff` with a unified diff built from the `FileEdit` lines (3 lines of context); without lines, old/new text from the `Target`/`Replacement`/`Content` arguments; compacted steps only have `+added -removed` |
| `read_file`, `view_file` | `Read` | result text; location with the first line |
| `search_web`, `search_marketplace` | `Search` | answer plus source links |
| `webview_browser` | `Fetch` | display markdown, screenshot |
| `call_mcp_tool` | `Mcp` | result text; `output` is the raw result (JSON when it parses) |
| `invoke_subagent`, `define_subagent`, `manage_subagents`, `send_message` | `Agent` | `child_thread_ids = ["antigravity:<child id>"]` for `invoke_subagent` |
| `update_focus` | `Think` | result text |
| others | `Other` | display markdown, else result text |

Title: display verb plus the first chip (`Ran <summary>`, `Edited <file name>`), else derived from the arguments (command line, file name, query, `server: tool`), else the tool name. Status: `Completed` for 3/5, `Failed` for 6/7 or any error details (the error text is appended as content). Images in results are inlined up to 256 KiB each; larger ones are replaced by a one-line note (tool-result images add up to 165 MB in the measured store).

### Item ids and turns

User and system steps: `step-<idx>`; planner text `step-<idx>-text`, thinking `step-<idx>-thinking`; tool calls use the native call id (`call_...`), which is also `ToolCall.call_id`; error notices `step-<idx>-error`. Every item after a user input (or a sub-agent's opening prompt) at step `n` gets turn id `turn-<n>`; items before the first input have no turn.
