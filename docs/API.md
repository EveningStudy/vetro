# UI protocol

Types are defined in `crates/vetro-proto` and generated for TypeScript into `apps/web/src/api/bindings` (`cargo test -p vetro-proto`). This file is the method catalogue.

## Transport

- `GET /api/health` -> `{ "ok": true, "version": "..." }` (no auth).
- `POST /api/auth/local` `{ "token": "..." }` -> `AuthSession`. Loopback only; used by the desktop window and by `vetro serve`'s printed link, which carry the local token in the URL fragment (`/#token=...`); the page removes it from the address bar.
- `POST /api/auth/pair` `{ "code": "...", "deviceName": "..." }` -> `AuthSession` for a new device.
- `GET /api/auth/session` with `Authorization: Bearer <session>` -> `200 { "authenticated": true, "remoteClient": bool }` or `401`.
- `GET /api/ws` -> WebSocket, JSON text frames:
  - request `{ "id": 1, "method": "threads.list", "params": { ... } }`
  - response `{ "id": 1, "result": { ... } }` or `{ "id": 1, "error": { "code": "not_found", "message": "..." } }`
  - event `{ "event": "thread.event", "data": { ... } }`

**No cookies.** Browsers send cookies to every port of a host, so a cookie for `127.0.0.1` would reach other local services. The client keeps the `session` string in origin-scoped storage (or in memory) and presents it explicitly: in `app.hello` on the WebSocket and as a bearer token on HTTP. A WebSocket that has not completed a valid `app.hello` within a few seconds is closed, and every other method is refused until then. `Origin` and `Host` are still checked on every request and upgrade.

After `app.hello`, requests can complete out of order while events continue to arrive. Match responses by `id`. If the server sends `{"event":"app.resync","data":{}}`, reload lists and reopen open threads.

Unknown fields must be ignored by both sides. Timestamps are milliseconds since the Unix epoch.

## Methods

| Method | Params | Result | Notes |
|---|---|---|---|
| `app.hello` | `HelloParams` | `AppInfo` | First call on every connection. |
| `agents.list` | `Empty` | `AgentsResult` | Cached detection. |
| `agents.refresh` | `Empty` | `AgentsResult` | Re-detect binaries, versions and auth state. |
| `agents.authenticate` | `AuthenticateParams` | `AuthenticateResult` | Runs the agent's own sign-in method. The result carries a URL and code when the flow continues in a browser elsewhere. |
| `agents.options` | `AgentOptionsParams` | `AgentOptionsResult` | Options and slash commands for a new thread, from Vetro's versioned cache of what the agent reported earlier. It never creates a native session by itself: when nothing is cached and the agent only reports options from a session, the result has `needsSession: true`, and a session is created only when the call carries `prepare: true` from an explicit user action; that session is then used for the thread the user starts next in that folder. |
| `projects.list` | `Empty` | `ProjectsResult` | Includes hidden projects. Missing directories sort last. |
| `projects.add` | `ProjectAddParams` | `Project` | Desktop client only. Adds a folder to Vetro's list. |
| `projects.remove` | `ProjectAddParams` | `Empty` | Desktop client only. Removes a manually added folder. Projects discovered from sessions can only be hidden. |
| `projects.update` | `ProjectUpdateParams` | `Project` | Vetro-local name, pin, hide. |
| `threads.list` | `ThreadsListParams` | `ThreadsListResult` | Newest first, paged. Linked children appear when `includeChildren` is true or `parentId` is supplied. Automated/internal sessions (`internal: true`) are omitted unless `includeInternal` is true. |
| `threads.open` | `ThreadOpenParams` | `ThreadOpenResult` | Loads items and subscribes to `thread.event`. A detached thread also gets its cached options, commands and saved choices. |
| `threads.close` | `ThreadIdParams` | `Empty` | Unsubscribes. Does not stop the agent. |
| `threads.items` | `ThreadItemsParams` | `ThreadItemsResult` | Older page. |
| `threads.start` | `ThreadStartParams` | `ThreadResult` | Creates the native session, sends the first message, subscribes. |
| `threads.send` | `ThreadSendParams` | `Empty` | Resumes the native session if needed. During a running turn: steers when the agent supports it, otherwise queues. |
| `threads.interrupt` | `ThreadIdParams` | `Empty` | |
| `threads.setOption` | `ThreadSetOptionParams` | `OptionsResult` | Model, mode, thinking level, ... |
| `threads.respond` | `ThreadRespondParams` | `Empty` | Answers a permission request or question. |
| `threads.update` | `ThreadUpdateParams` | `ThreadResult` | Vetro-local title and archive flag. |
| `threads.detach` | `ThreadIdParams` | `Empty` | Releases the agent process for this thread. |
| `threads.raw` | `ThreadRawParams` | `ThreadRawResult` | Per-thread wire log. The limit preserves launch and handshake entries. |
| `search.threads` | `SearchParams` | `SearchResult` | Literal, case-insensitive full-text phrase search over titles and user and agent messages. Supports Chinese substrings. Returns one ranked hit per unarchived thread, with a matching snippet. Snippet text is HTML-escaped and matches use `<mark>` tags. |
| `fs.list` | `FsListParams` | `FsListResult` | Read-only folder picker. Remote clients are limited to known project roots. |
| `fs.find` | `FsFindParams` | `FsFindResult` | Read-only file mentions. Remote clients are limited to known project roots. |
| `settings.get` | `Empty` | `Settings` | |
| `settings.update` | `SettingsUpdateParams` | `Settings` | Desktop client only. |
| `prefs.get` | `Empty` | `Prefs` | UI preferences of the calling device, stored by the core so they survive a WebView without persistent storage. |
| `prefs.set` | `Prefs` | `Empty` | |
| `remote.status` | `Empty` | `RemoteStatus` | |
| `remote.set` | `RemoteSetParams` | `RemoteStatus` | Desktop client only. |
| `remote.createPairing` | `Empty` | `Pairing` | Desktop client only. |
| `remote.revokeDevice` | `RemoteRevokeParams` | `RemoteStatus` | Remote clients can revoke their own device. |

## Events

`ServerEvent` in `vetro-proto`:

- `thread.event`: for opened threads only; `seq` increases by one per thread. `ThreadEvent` is `item_added`, `item_updated`, `text_delta`, `summary`, `options`, `commands` or `usage`.
- `threads.upserted` / `threads.removed`: the thread list changed (new session, title, status, link, history refresh). Sent to every connection.
- `agents.updated`, `projects.updated`, `index.progress`, `remote.updated`.

Listings and searches use the local index immediately. During a rebuild its initial results can be incomplete. `index.progress` reports a scan per agent. `done` means the scan ended, including sessions whose transcripts require sign-in. `threads.upserted` carries newly indexed summaries and changes to links. Open history threads receive item events as their native transcripts change.

## Content rules

- A `text` block is what the user typed, unchanged, including `@path` mentions and `/commands`.
- `file` blocks accompany a mention as a hint. Adapters map them to the agent's native mention type when one exists and ignore them otherwise; a message is never rejected because of them.
- `projects.list` returns hidden projects too; clients filter on `hidden`.

## Client rules

- After `threads.open`, apply only events with `seq` greater than the returned `seq`. On a gap or a reconnect, open the thread again and replace the local copy.
- `text_delta` appends to the `text` of an `agent_message` or `reasoning` item whose `streaming` is true; the final text arrives with `item_updated` and `streaming: false`.
- Items are ordered by `ordinal`. An `item_updated` for an unknown id is treated as `item_added`.
- A `request` item with `resolution: null` is pending; the thread status is `awaiting_input` while any is pending.
