# ACP v1 adapter notes

`verre_agent_acp::AcpAgent` implements `verre_agent_api::Agent` and returns
`Arc<dyn Session>`. Use `antigravity()` for `agy acp`, `devin()` for `devin acp --process-model in-process`,
or `new(id, name, SpawnSpec)` to specify a native executable, arguments,
environment overrides and an optional process working directory. The core
also uses `--process-model in-process` for the built-in Devin agent, keeping
its sessions in the child process Verre owns. On Windows, the core checks the
installed Devin Desktop application's uninstall registry entries.
`with_mid_turn_input` enables steering for a custom agent that supports it.
`wire_log()` inspects the currently started process.

`options()` creates a session without sending a prompt and caches the reported
options and available commands per process. It closes the probe if the agent
advertises `close` and deletes it if the agent advertises `delete`. Agents with
neither capability retain the probe in their native store. An agent can publish
commands asynchronously after the probe has closed. Such commands are available
through live session events when they arrive before the live session closes.

`read()` uses the history record's cwd when available and captures replay in
wire order up to the `session/load` reply. It closes
the loaded session when the agent supports `close`. Loading an already attached
session returns `Busy` to protect its live turn. `attach()` prefers
`session/resume` and uses `session/load` where only replay-based continuation is
advertised. Core callers can use `read()` to build the history and then attach
without emitting that replay a second time. `shutdown()` releases the shared
process. The next agent call starts a fresh one after shutdown or a crash.
Devin requires native browser authentication for each fresh process.
`verre probe devin --auth` selects the first advertised method. The wire log
records environment override names and masks their values.

## Contract gaps

- The shared JSON-RPC peer delivers replies and notifications on separate
  channels. ACP needs a single wire-order boundary for replay and turn
  completion. This adapter uses the shared `Process`, Windows Job Object or
  Unix process group, and a small ordered JSON-RPC dispatcher.
- `SessionEvent` represents session titles, models, usage and text, while ACP
  can also report session timestamps, cumulative cost, and rich agent content.
  Session info timestamps are available on list records, context token counts
  map to `ThreadUsage`, and supported non-text agent content is carried as a
  notice. Terminal references require a client terminal capability, which
  Verre disables.
- `RequestBody` supports permission choices and question lists. The adapter
  advertises form elicitation and translates session-scoped fields and typed
  answers. URL and request-scoped elicitation, including forms opened outside a
  session during authentication, have no corresponding contract UI. Forms
  containing unsupported field types are declined with invalid params.
- ACP turn completion, cancellation and failures arrive asynchronously. `send`
  returns once the prompt is written. Turn completion and errors are reported
  in events. When a prompt is rejected asynchronously for authentication, the
  adapter emits `AuthRequired`. New-session authentication errors and sends to
  a process already known to require authentication return `AuthRequired` directly.
  ACP v1 requires a base64 payload even when an image URI is present. The
  caller can supply inline data or a base64 data URI.

## Live capture on November 23, 2026

The log excerpts remove session identifiers, local paths, transcript contents,
and large capability payloads.

### Antigravity 1.5.14

```text
> initialize {"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false,"elicitation":{"form":{}}}}
< initialize {"protocolVersion":1,"loadSession":true,"sessionCapabilities":{"additionalDirectories":{},"close":{},"list":{},"load":{},"resume":{}},"authMethods":["oauth-personal","oauth-business","gemini-api-key","agent-platform"]}
> session/new {"mcpServers":[]}
< session/new {"configOptions":[{"id":"model","choices":9},{"id":"thought_level","choices":4},{"id":"mode","choices":4}]}
> session/close {}
< session/close {}
> session/list {}
< session/update "available_commands_update"
< session/list {"count":50,"hasCursor":false}
```

The handshake passed. The installed version also emitted `load_session` and
`session_capabilities` alongside their standard camelCase counterparts, plus
an extra `sessionCapabilities.load` marker. Verre uses `loadSession` for
replay capability. The agent advertised a system-prompt append extension in
`_meta`. The adapter did not invoke that extension or send a prompt.

### Devin 3000.14.30833

```text
> initialize {"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false,"elicitation":{"form":{}}}}
< initialize {"protocolVersion":1,"loadSession":true,"sessionCapabilities":{"close":{},"delete":{},"list":{}},"authMethods":["devin-browser"]}
> session/list {}
< session/list {"count":9,"hasCursor":false}
> session/new {"mcpServers":[]}
< session/new {"errorCode":-32000}
> authenticate {"methodId":"devin-browser"}
< _cognition.ai/mcp/serversChanged
< authenticate {}
> session/new {"mcpServers":[]}
< session/update "config_option_update"
< session/update "available_commands_update"
< session/new {"mode":"accept-edits","modes":["accept-edits","smart","ask","plan","bypass"],"configOptions":[{"id":"mode","choices":5},{"id":"model","choices":170},{"id":"thought_level","choices":3},{"id":"speed","choices":2}]}
> session/set_config_option {"configId":"model","value":"swe-2-flash-medium"}
< session/set_config_option {}
> session/prompt {"contentBlocks":1}
< session/update "agent_message_chunk"
< _cognition.ai/turn_stats
< session/prompt {"stopReason":"end_turn"}
> session/close {}
< session/close {}
> session/load {"mcpServers":[]}
< session/update "user_message_chunk"
< session/update "agent_message_chunk"
< session/load {"hasSessionId":false,"mode":"accept-edits"}
> session/close {}
< session/close {}
> session/load {"mcpServers":[]}
< session/load {"hasSessionId":false,"mode":"accept-edits"}
> session/prompt {"contentBlocks":1}
< session/update "tool_call" {"kind":"execute","status":null}
< session/update "tool_call_update" {"status":"in_progress"}
< session/update "tool_call_update" {"status":"completed"}
< session/prompt {"stopReason":"end_turn"}
```

The installed Devin version requires native browser authentication per process
for `session/new`. Listing works before authentication. Native loading replayed
the first prompt and `OK` response, and a separate load restored the session for
the tool turn. Devin advertises steering through `_meta` key
`cognition.ai/midTurnPrompts` and sends proprietary notifications such as
`_cognition.ai/mcp/serversChanged`, `thinking_complete`, `turn_stats`, and
`agent_stopped`. It can emit new-session updates before the response containing
the session ID. A live `tool_call` patch omitted its status, so the adapter used
`Pending` until the next update. The permission assertion failed on the live
run because the default `accept-edits` mode let `echo OK` complete without
requesting permission. The test now selects the agent-reported `ask` mode
before its tool turn. The revised live assertion remains unverified after the
single requested run. The fake agent verifies the permission round trip.
