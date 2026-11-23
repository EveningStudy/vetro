# UI protocol

Types are defined in `crates/vetro-proto` and generated for TypeScript into `apps/web/src/api/bindings` (`cargo test -p vetro-proto`). This file is the method catalogue.

## Transport

- `GET /api/health` -> `{ "ok": true, "version": "..." }` (no auth).
- `POST /api/auth/local` `{ "token": "..." }` -> sets the session cookie. Loopback only; used by the desktop window, which receives the token in the URL fragment (`/#token=...`) and removes it from the address bar.
- `POST /api/auth/pair` `{ "code": "...", "deviceName": "..." }` -> sets the session cookie for a new device.
- `GET /api/auth/session` -> `200 { "authenticated": true, "remoteClient": bool }` or `401`.
- `GET /api/ws` -> WebSocket (cookie required). JSON text frames:
  - request `{ "id": 1, "method": "threads.list", "params": { ... } }`
  - response `{ "id": 1, "result": { ... } }` or `{ "id": 1, "error": { "code": "not_found", "message": "..." } }`
  - event `{ "event": "thread.event", "data": { ... } }`

Unknown fields must be ignored by both sides. Timestamps are milliseconds since the Unix epoch.

## Methods

| Method | Params | Result | Notes |
|---|---|---|---|
| `app.hello` | `HelloParams` | `AppInfo` | First call on every connection. |
| `agents.list` | `Empty` | `AgentsResult` | Cached detection. |
| `agents.refresh` | `Empty` | `AgentsResult` | Re-detect binaries, versions and auth state. |
| `agents.authenticate` | `AuthenticateParams` | `Empty` | Runs the agent's own sign-in method. |
| `agents.options` | `AgentOptionsParams` | `AgentOptionsResult` | Options and slash commands for a new thread, from the agent (cached). |
| `projects.list` | `Empty` | `ProjectsResult` | |
| `projects.add` | `ProjectAddParams` | `Project` | Adds a folder to Vetro's list only. |
| `projects.update` | `ProjectUpdateParams` | `Project` | Vetro-local name, pin, hide. |
| `threads.list` | `ThreadsListParams` | `ThreadsListResult` | Newest first, paged. |
| `threads.open` | `ThreadOpenParams` | `ThreadOpenResult` | Loads items and subscribes to `thread.event`. |
| `threads.close` | `ThreadIdParams` | `Empty` | Unsubscribes. Does not stop the agent. |
| `threads.items` | `ThreadItemsParams` | `ThreadItemsResult` | Older page. |
| `threads.start` | `ThreadStartParams` | `ThreadResult` | Creates the native session, sends the first message, subscribes. |
| `threads.send` | `ThreadSendParams` | `Empty` | Resumes the native session if needed. During a running turn: steers when the agent supports it, otherwise queues. |
| `threads.interrupt` | `ThreadIdParams` | `Empty` | |
| `threads.setOption` | `ThreadSetOptionParams` | `OptionsResult` | Model, mode, thinking level, ... |
| `threads.respond` | `ThreadRespondParams` | `Empty` | Answers a permission request or question. |
| `threads.update` | `ThreadUpdateParams` | `ThreadResult` | Vetro-local title and archive flag. |
| `threads.detach` | `ThreadIdParams` | `Empty` | Releases the agent process for this thread. |
| `threads.raw` | `ThreadRawParams` | `ThreadRawResult` | Wire log. |
| `search.threads` | `SearchParams` | `SearchResult` | Full-text over titles and message text. |
| `fs.list` | `FsListParams` | `FsListResult` | Folder picker. Read-only. |
| `fs.find` | `FsFindParams` | `FsFindResult` | File mentions. Read-only. |
| `settings.get` | `Empty` | `Settings` | |
| `settings.update` | `SettingsUpdateParams` | `Settings` | |
| `remote.status` | `Empty` | `RemoteStatus` | |
| `remote.set` | `RemoteSetParams` | `RemoteStatus` | Desktop client only. |
| `remote.createPairing` | `Empty` | `Pairing` | Desktop client only. |
| `remote.revokeDevice` | `RemoteRevokeParams` | `RemoteStatus` | |

## Events

`ServerEvent` in `vetro-proto`:

- `thread.event`: for opened threads only; `seq` increases by one per thread. `ThreadEvent` is `item_added`, `item_updated`, `text_delta`, `summary`, `options`, `commands` or `usage`.
- `threads.upserted` / `threads.removed`: the thread list changed (new session, title, status, link, history refresh). Sent to every connection.
- `agents.updated`, `projects.updated`, `index.progress`, `remote.updated`.

## Client rules

- After `threads.open`, apply only events with `seq` greater than the returned `seq`. On a gap or a reconnect, open the thread again and replace the local copy.
- `text_delta` appends to the `text` of an `agent_message` or `reasoning` item whose `streaming` is true; the final text arrives with `item_updated` and `streaming: false`.
- Items are ordered by `ordinal`. An `item_updated` for an unknown id is treated as `item_added`.
- A `request` item with `resolution: null` is pending; the thread status is `awaiting_input` while any is pending.
