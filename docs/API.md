# UI protocol

Types are defined in `crates/verre-proto` and generated for TypeScript into `apps/web/src/api/bindings` (`cargo test -p verre-proto`). This file is the method catalogue.

## Transport

- `GET /api/health` -> `{ "ok": true, "version": "..." }` (no auth).
- `POST /api/auth/local` `{ "token": "..." }` -> `AuthSession`. Loopback only; used by the desktop window and by `verre serve`'s printed link, which carry the local token in the URL fragment (`/#token=...`); the page removes it from the address bar.
- `POST /api/auth/pair` `{ "code": "...", "deviceName": "...", "proposedSession": "..." }` -> `AuthSession` for a new or recovered device. `proposedSession` is optional. Clients that supply it must use cryptographically random 16-byte and 32-byte values for the ID and secret, respectively, written as lowercase hexadecimal and separated by a dot. Keep it private like any session credential. Clients that omit it receive a server-generated session.
- `GET /api/auth/session` with `Authorization: Bearer <session>` -> `200 { "authenticated": true, "remoteClient": bool }` or `401`.
- `GET /api/ws` -> WebSocket, JSON text frames:
  - request `{ "id": 1, "method": "threads.list", "params": { ... } }`
  - response `{ "id": 1, "result": { ... } }` or `{ "id": 1, "error": { "code": "not_found", "message": "..." } }`
  - event `{ "event": "thread.event", "data": { ... } }`

**No cookies.** The client presents its `session` string in `app.hello` on the WebSocket and as a bearer token on HTTP. Local sessions use `sessionStorage`. When the user keeps **Remember this browser** selected during HTTPS pairing, the web client uses origin-scoped `localStorage` so a new tab can reconnect. A temporary pairing uses `sessionStorage`. Both modes can fall back to memory if browser storage is unavailable. The client drops a rejected credential after HTTP `401`. A WebSocket must complete a valid `app.hello` within a few seconds, and every other method is refused until then. `Origin` and `Host` are still checked on every request and upgrade.

The web client gives each authentication request 15 seconds, including reading the response body. The local retry action keeps the launch token available across temporary errors. The pairing form keeps the code, device name and proposed session after a timeout. The first successful pairing consumes the code. Repeating the same proposal can recover that device while its session is valid, and a different proposal still needs a valid unused code. The host persists the device ID and a hash of its secret.

After `app.hello`, requests can complete out of order while events continue to arrive. Match responses by `id`. If the server sends `{"event":"app.resync","data":{}}`, reload lists and reopen open threads.

Supply a unique `clientMessageId` on `threads.start` and `threads.send`. IDs are scoped to the originating device and, for `threads.send`, to the thread. After a lost response, repeat the request with the original ID and `recoverOnly: true` to check the result. A confirmed start returns the original thread. A confirmed send returns the original acknowledgement. The host retains up to 256 start attempts and 256 accepted sends per retained thread in memory. If a receipt is missing, including after a host restart, recovery returns `delivery_unknown` without creating a session or submitting a message. Check the thread history before sending again. Use a fresh ID for each new message, including repeated text.

Unknown fields must be ignored by both sides. Timestamps are milliseconds since the Unix epoch.

## Methods

| Method | Params | Result | Notes |
|---|---|---|---|
| `app.hello` | `HelloParams` | `AppInfo` | First call on every connection. |
| `app.ping` | `Empty` | `Empty` | Checks that an authenticated connection can still receive replies. |
| `agents.list` | `Empty` | `AgentsResult` | Cached detection. |
| `agents.refresh` | `Empty` | `AgentsResult` | Re-detect binaries, versions and auth state, and refresh official rate limits where available. |
| `agents.authenticate` | `AuthenticateParams` | `AuthenticateResult` | Runs the agent's own sign-in method. A result with a URL or code requires a follow-up step. The local client checks the agent's sign-in status when the window regains focus. |
| `agents.options` | `AgentOptionsParams` | `AgentOptionsResult` | Options and slash commands for a new thread, from Verre's versioned cache of what the agent reported earlier. It never creates a native session by itself: when nothing is cached and the agent only reports options from a session, the result has `needsSession: true`, and a session is created only when the call carries `prepare: true` from an explicit user action; that session is then used for the thread the user starts next in that folder. |
| `projects.list` | `Empty` | `ProjectsResult` | Includes hidden projects. Missing directories sort last. |
| `projects.add` | `ProjectAddParams` | `Project` | Desktop client only. Adds a folder to Verre's list. |
| `projects.remove` | `ProjectAddParams` | `Empty` | Desktop client only. Removes a manually added folder. Projects discovered from sessions can only be hidden. |
| `projects.update` | `ProjectUpdateParams` | `Project` | Verre-local name, pin, hide. |
| `threads.list` | `ThreadsListParams` | `ThreadsListResult` | Newest first, paged. Linked children appear when `includeChildren` is true or `parentId` is supplied. Automated/internal sessions (`internal: true`) are omitted unless `includeInternal` is true. |
| `threads.active` | `{}` | `ThreadsListResult` | Running root threads and roots awaiting input, independent of history filters and pagination. |
| `threads.open` | `ThreadOpenParams` | `ThreadOpenResult` | Loads items and subscribes to `thread.event`. A detached thread also gets its cached options, commands and saved choices. |
| `threads.close` | `ThreadIdParams` | `Empty` | Unsubscribes. Does not stop the agent. |
| `threads.items` | `ThreadItemsParams` | `ThreadItemsResult` | Older page. |
| `threads.start` | `ThreadStartParams` | `ThreadResult` | Creates the native session, sends the first message, subscribes. |
| `threads.fork` | `ThreadForkParams` | `ThreadResult` | Forks an idle native session and subscribes to the new one. The new session reports its own effective options. A checkpoint requires native support. |
| `threads.send` | `ThreadSendParams` | `Empty` | Resumes the native session if needed. During a running turn: steers when the agent supports it, otherwise queues. |
| `threads.interrupt` | `ThreadIdParams` | `Empty` | |
| `threads.setOption` | `ThreadSetOptionParams` | `OptionsResult` | Model, mode, thinking level, ... |
| `threads.respond` | `ThreadRespondParams` | `Empty` | Answers a permission request or question. |
| `threads.update` | `ThreadUpdateParams` | `ThreadResult` | Verre-local title, archive and pin flags. Never modifies the agent's own store. |
| `threads.detach` | `ThreadDetachParams` | `Empty` | Detaches this native session and releases its runtime once no other live session or agent operation needs it. Fails with `background_work` while the runtime has live descendants that appeared after it began processing prompts, unless `force` is `true`. |
| `threads.rollback` | `ThreadRollbackParams` | `ThreadOpenResult` | Requires a resumable, idle thread with `historyEdit.rollbackConversation` support. Excludes the specified native turn and all later turns from the conversation. Returns a fresh snapshot and subscribes the caller. Other views refresh on `app.resync`. |
| `threads.rewindFiles` | `ThreadRewindFilesParams` | `RewindFilesResult` | Requires a resumable, idle thread and a native user-message checkpoint UUID. `dryRun: true` previews the changes. Applying restores tracked files through the agent and can report skipped links. Check `historyEdit.rewindFiles` and `historyEdit.previewRewindFiles` for support. |
| `threads.raw` | `ThreadRawParams` | `ThreadRawResult` | Per-thread wire log. The limit preserves launch and handshake entries. |
| `search.threads` | `SearchParams` | `SearchResult` | Literal, case-insensitive full-text phrase search over titles and user and agent messages. Supports Chinese substrings. Returns one ranked hit per unarchived thread, with a matching snippet. Snippet text is HTML-escaped and matches use `<mark>` tags. |
| `fs.list` | `FsListParams` | `FsListResult` | Read-only folder picker. Remote clients are limited to known project roots. |
| `fs.find` | `FsFindParams` | `FsFindResult` | Read-only file mentions. Remote clients are limited to known project roots. |
| `workspace.gitStatus` | `WorkspacePathParams` | `GitStatusResult` | Read-only Git branch and changed-file summary (`--no-optional-locks`). Remote clients are limited to known project roots. |
| `workspace.gitDiff` | `GitDiffParams` | `GitDiffResult` | Read-only Git diff for one changed file. Remote clients are limited to known project roots. |
| `workspace.readFile` | `WorkspaceFileParams` | `WorkspaceFileResult` | Read-only preview of a regular file. UTF-8 source (including SVG) is capped at 256 KiB. Supported raster images are capped at 4 MiB. Remote clients supply a known workspace and relative file path and receive relative paths. On desktop, an empty `cwd` allows an absolute or `~`-expanded path. |
| `workspace.worktrees` | `WorkspacePathParams` | `WorktreesResult` | Read-only list of registered Git worktrees. Remote clients are limited to known project roots for the requested checkout. |
| `workspace.openExternal` | `OpenExternalParams` | `Empty` | Desktop local client only. Opens a workspace folder, terminal, or file in the user's configured external app. |
| `app.notify` | `NotifyParams` | `Empty` | Desktop local client only. Requests a native OS desktop notification when enabled in settings. |
| `app.setBadge` | `BadgeParams` | `Empty` | Desktop local client only. Updates the taskbar overlay or Dock/launcher badge count. |
| `app.checkUpdate` | `Empty` | `UpdateCheckResult` | Checks for newer Verre desktop releases (signed Tauri manifest or GitHub Releases). |
| `app.installUpdate` | `UpdateInstallParams` | `UpdateInstallResult` | Desktop local client only. Fails with `background_work` if sessions or background work are active unless `force` is `true`. Installs a signed desktop update with `update.progress` events and restarts, or opens the release page. |
| `settings.get` | `Empty` | `Settings` | |
| `settings.update` | `SettingsUpdateParams` | `Settings` | Desktop client only. |
| `prefs.get` | `Empty` | `Prefs` | UI preferences of the calling device, stored by the core so they survive a WebView without persistent storage. |
| `prefs.set` | `Prefs` | `Empty` | |
| `remote.status` | `Empty` | `RemoteStatus` | A remote client sees its own device. Only local clients see the pending pairing expiry. |
| `remote.set` | `RemoteSetParams` | `RemoteStatus` | Local client only. Enable with `tailscaleServe: true` or `externalUrl` set to an HTTPS origin. Disable with `enabled: false`. |
| `remote.createPairing` | `Empty` | `Pairing` | Desktop client only. |
| `remote.revokeDevice` | `RemoteRevokeParams` | `RemoteStatus` | Remote clients can revoke their own device. |
| `voice.warmup` | `Empty` | `Empty` | Preloads the configured local voice model in memory when recording begins. |

## Events

`ServerEvent` in `verre-proto`:

- `thread.event`: for opened threads only; `seq` increases by one per thread. `ThreadEvent` is `item_added`, `item_updated`, `text_delta`, `summary`, `options`, `commands` or `usage`.
- `threads.upserted` / `threads.removed`: the thread list changed (new session, title, status, link, history refresh). Sent to every connection.
- `agents.changed`: a single agent with a newly observed official rate-limit snapshot. The core also broadcasts the updated agent list via `agents.updated`.
- `agents.updated`, `projects.updated`, `index.progress`, `remote.updated`, `app.navigate`, `voice.progress`, `update.progress`.
- `app.resync`: replace cached lists and snapshots of open threads after a history replacement or missed events.

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
