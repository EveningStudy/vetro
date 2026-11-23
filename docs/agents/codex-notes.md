# Codex app-server notes

## Installed protocol

Verified on Windows with `codex-cli 0.169.0`. The npm installation puts the
native executable in a platform package under `@openai/codex/node_modules`.
Vetro launches that executable with `app-server` through the shared process
transport and Windows Job Object.

The local reference under `.orchestra/ref/codex-schema` was generated with
`app-server generate-json-schema --out` and `app-server generate-ts --out`.
The `experimental` subdirectory contains the same schema with `--experimental`
for collaboration presets, permission profile selection, and approval choices.

Trimmed handshake and history probe from the installed server:

```text
> {"id":1,"method":"initialize","params":{"clientInfo":{"name":"vetro","title":"Vetro","version":"0.1.0"},"capabilities":{"experimentalApi":true}}}
< {"id":1,"result":{"userAgent":"vetro/0.169.0 (…)","platformOs":"windows",…}}
> {"method":"initialized"}
> {"id":2,"method":"thread/list","params":{"limit":3,"modelProviders":[],"sourceKinds":["cli","vscode","exec","appServer","subAgent","subAgentReview","subAgentCompact","subAgentThreadSpawn","subAgentOther","unknown"],"useStateDbOnly":true}}
< {"id":2,"result":{"data":[{"originator":"codex_exec","source":"exec",…},…],"nextCursor":"<cursor>"}}
```

- Stdio uses one JSON message per line and bare envelopes. A numeric or string
  server request ID can be returned verbatim in the response envelope.
- `initialize.clientInfo.name` became `originator: "vetro"` on live scratch
  threads, which the server classified with `source: "vscode"`. Older
  sub-agent records can contain `source.subagent.thread_spawn.parent_thread_id`.
- `thread/list` defaults to interactive sources. Vetro sets all known source
  kinds and `modelProviders: []` on every cursor page. `useStateDbOnly` keeps the
  listing from scanning rollouts to repair metadata. Archived history is outside
  this listing because `SessionRecord` has no archived state.
- `thread/start` accepts named `permissions` profiles. It has no reasoning or
  collaboration fields in this version. User-selected effort and collaboration
  mode go on the first `turn/start`. Later edits use `thread/settings/update`.
- `model/list` returned the current provider's catalog. Its reasoning efforts
  are per model. It reports neither prices nor a cheapest-model flag, so the
  live test uses catalog affordability descriptions and the lowest offered effort.
- `permissionProfile/list` returned `:read-only`, `:workspace`, and
  `:danger-full-access`, all allowed on this installation. `collaborationMode/list`
  returned Plan and Default. `configRequirements/read` returned null allowlists
  for approval policies and login methods.
- `account/read` reported a ChatGPT account alongside the custom model catalog.
  `account/rateLimits/read` returned a snapshot. Rolling notifications can omit
  previously observed fields, so the adapter merges snapshots by limit ID.

## Validation

The fake app-server verifies bare envelopes, the exact prompt payload, explicit
option forwarding, command approval, live terminal output, steering, interruption,
usage, history paging and reading, resume, and recovery after process exit.
An excerpt from its approval exchange with the adapter:

```text
< {"id":"server-command","method":"item/commandExecution/requestApproval","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"command","availableDecisions":["accept","decline"]}}
> {"id":"server-command","result":{"decision":"accept"}}
< {"method":"item/commandExecution/outputDelta","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"command","delta":"approved\n"}}
```

The November 23, 2026 live run read 45 items from an existing `exec` thread and
listed two 50-thread pages. The scratch thread answered `OK`, resumed by ID,
and returned the same answer on readback. The second live turn received a
permission interaction whose displayed detail did not contain the expected
scratch marker. The test stopped before replying, and a read-only probe later
reported that turn as interrupted. The original approval envelope was unavailable
after exit. The test now also checks the associated command item and prints a
trimmed approval envelope on failure. Those diagnostics are pending a future
live run.

## Contract gaps

- `Agent::authenticate` returns only success or error. The browser-based
  ChatGPT flow fits this interface. Device-code login needs a way to show its
  verification URL and one-time code. API key and externally managed token modes
  require credentials supplied by the client, so those stay in Codex's own
  sign-in flow.
- `Question` has no secret or required-field marker and no structured JSON
  schema field. The adapter supports ordinary user-input questions and primitive
  MCP form fields, and rejects requests for secret input. URL elicitations surface
  the URL in the request detail. `RequestBody` has no dedicated browser action.
- `Agent::options` has no provisional model argument. The initial effort list
  follows the default model. An attached session refreshes the effort choices
  when the model changes.
- The shared transport logs complete wire envelopes in memory and has no
  per-method redaction. The adapter starts the native browser flow and does not
  submit credentials. The wire excerpts here omit account details, local paths,
  provider model IDs, and history contents.
