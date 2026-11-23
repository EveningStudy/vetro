# Claude Code adapter notes

Verified on Windows with Claude Code 2.1.330. The adapter uses the installed executable and reads transcripts from `~/.claude/projects` or the `projects` directory under `CLAUDE_CONFIG_DIR`. References were the `@anthropic-ai/claude-agent-sdk` 0.3.330 npm package (`sdk.d.ts`, `core.mjs`) and the official Python SDK (`_internal/transport/subprocess_cli.py`, `_internal/query.py`). The adapter has no SDK dependency.

## Process and control channel

```text
claude -p --input-format stream-json --output-format stream-json --verbose
  --include-partial-messages --permission-prompts host --permission-prompt-tool=stdio
  --include-hook-events --replay-user-messages --forward-subagent-text
  (--session-id <uuid> | --resume <uuid>)
```

`--permission-prompts host` accepted `initialize` by itself, but the native `Write` request was denied by the CLI without a host round-trip. Adding the SDK's `--permission-prompt-tool=stdio` produced `can_use_tool` with a native permission suggestion. `--replay-user-messages` gives user UUID acknowledgments. `--forward-subagent-text` asks the CLI to include subagent text and thinking with its parent tool-use ID. Start-time model, mode and effort flags require an explicit user choice. There are no environment overrides. The CLI recorded these runs as `sdk-cli`, mapped to `Exec` with the raw label retained.

The handshake carries only `subtype: initialize`. Its reply supplies models, command metadata, the CLI version, permission mode, and account information. `Agent::info` exposes the subscription type or provider label. The `system/init` frame supplies the effective model and the native session ID, command names and tools. Raw frames are available in the session wire log. Both startup and `--resume` emitted the installed SessionStart hook's `ai-memory` output. Model and mode changes were acknowledged through `set_model` and `set_permission_mode`. Authentication is performed in the user’s Claude Code terminal.

## Trimmed native wire

IDs below are replaced with placeholders. Hook and initialization payloads are reduced to the fields shown. The markers in brackets summarize data present in the wire.

```text
> {"type":"control_request","request_id":"<init>","request":{"subtype":"initialize"}}
< {"type":"system","subtype":"hook_started","hook_event":"SessionStart"}
< {"type":"system","subtype":"hook_response","hook_event":"SessionStart",…}  [output contained ai-memory]
< {"type":"control_response","response":{"subtype":"success","request_id":"<init>","response":{…}}}  [14 models, 71 commands]
> {"type":"user",…,"message":{"role":"user","content":[{"type":"text","text":"Reply with exactly: OK"}]}}
< {"type":"system","subtype":"init","model":"claude-haiku-5-5",…}  [31 tools, 71 slash commands]
< {"type":"user","isReplay":true,…}
< {"type":"stream_event","user_message_uuid":"<prompt>","event":{"type":"message_start",…},…}
< {"type":"result","subtype":"success","stop_reason":"end_turn","duration_ms":1731,…}
  [next turn asks the Write tool to create a scratch file]
< {"type":"control_request","request_id":"<ask>","request":{"subtype":"can_use_tool","tool_name":"Write",…}}  [one suggestion]
> {"type":"user",…,"message":{"role":"user","content":[{"type":"text","text":"Reply with exactly: QUEUE"}]}}
> {"type":"control_response","response":{"subtype":"success","request_id":"<ask>","response":{"behavior":"deny",…}}}
< {"type":"user","isReplay":true,…}
< {"type":"stream_event","event":{"type":"message_start",…},…}
< {"type":"result","subtype":"success","stop_reason":"end_turn","duration_ms":2166,…}
```

The last two prompts produced one result. After the permission was answered, the CLI consumed the second prompt at the tool boundary and replied `QUEUE` during the original turn. The adapter reports `mid_turn_input` for this behavior. Delivery during uninterrupted text generation and the timing at other tool boundaries were not measured. The fake verifies that a replay acknowledgment for a folded prompt clears the extra pending entry when that turn ends.

Permission answers reference the original request ID. Allow-once returns the original `updatedInput`. Always-allow returns the CLI's own `permission_suggestions` as `updatedPermissions` when offered. `AskUserQuestion` returns `updatedInput.answers` keyed by question text. Plan approval is an interaction request. Denial, interrupt, and cancellations update the timeline. Always-allow, questions, plan approval, interrupt, compaction, rate limits, and subagent forwarding use synthetic coverage. The native test deliberately denies the request to avoid creating a file or storing a permission rule.

`result.usage` covers the main agent loop for that turn. The cumulative `result.modelUsage` includes subagents and auxiliary calls. The adapter uses it for the session total when available.

## History and contract gaps

The history listing scans top-level project JSONL files and skips large payload rows when it only needs metadata. It uses the CLI's title or summary records, then the first user prompt. The transcript reader reuses the live item mapping and associates subagent sidecars through their `toolUseId` or the parent tool result's `agentId`. Torn JSONL tails are skipped. The read-only native check listed 121 sessions in 4.14 seconds in the debug profile (119 `claude-desktop`, two `sdk-cli`). The largest file was 36,568,864 bytes and yielded 970 items in 0.46 seconds. The native `OK` assistant message had the same item ID in the live stream and after reading its transcript.

- `ToolCall` lacks typed child items. Claude Agent calls carry their nested timeline in `output.subagentItems` and preserve the native result in `output.result`.
- `SessionEvent` has no tool inventory event, so the inventory is visible in the raw `system/init` frame. `Session::native_id()` cannot represent an ID changed by a native session reset.
- `ConfigOption` has no launch-only attribute, so the effort option has an explanatory description. This adapter applies effort at launch.
- Claude's message shape provides text and images. The adapter returns a bad-request error for a structured local file mention because it has no native representation that preserves the original prompt.
