# Security policy

## What Vetro can access

Vetro is a local process. It starts the coding-agent CLIs installed on your
machine (Claude Code, Codex, Kiro, Antigravity, Devin, and custom ACP agents) as
child processes with your own permissions, and it serves the UI on `127.0.0.1`
only. Everything Vetro stores goes under `~/.vetro` (`VETRO_HOME` overrides it);
the agents' own session stores are opened read-only. Vetro never silently writes
hidden config into project workspaces — only explicit, user-triggered actions
touch a workspace.

Vetro never sees agent credentials: sign-in happens inside each agent's own
flow. Remote access is exclusively the user's own Tailscale network (via
`tailscale serve` or a reverse proxy the user runs) — Vetro never binds a
public interface, and every request requires the local token or a paired-device
bearer session token (`Authorization: Bearer` on HTTP and `app.hello` on
WebSocket; Vetro sets no cookies).

## Local data & raw wire logs (`~/.vetro`)

- **`config.json`**: application settings (`Settings`), including agent paths,
  custom ACP commands, voice config, and notification preferences.
- **`state.json`**: local bootstrap token, SHA-256 hashes of local/paired
  session tokens (plaintext paired tokens are never stored), per-device UI
  preferences, thread/project overlays, cached agent options, and rate-limit
  snapshots.
- **`index.db`**: SQLite + FTS5 index of thread summaries, parent/child link
  evidence, and searchable message text. Disposable; deleting it rebuilds from
  the agents' native stores.
- **`raw/<sha256(thread_id)>.jsonl`**: wire log (`spawn`, `sent`, `received`,
  `stderr`) for sessions attached in Vetro:
  - Kept in a 4 MiB per-session ring buffer in memory; oldest entries are
    dropped first while preserving the initial launch/handshake prefix.
    `threads.raw` defaults to 500 lines (max 5,000).
  - **Redaction**: `spawn` entries replace all environment override values with
    `<redacted>`; Codex multiplexed `config/read` responses are replaced with
    `{"id": ..., "redacted": true}`. Regular prompts, agent replies, and tool
    payloads are recorded verbatim so you can verify zero injection.
- **`logs/core.log`**: diagnostic core log rotated to a 256 KiB rolling window.
- On Unix, `~/.vetro` directories use mode `0700` and files use mode `0600`.

## Paired-device (remote) boundaries

Paired remote browsers share the UI over HTTPS with strict host-only guards:

- **Allowed**: list/search/open/fork/rollback/rewind threads, send/steer/queue
  or interrupt turns, answer approvals/questions, inspect raw logs, view
  read-only Git status/diff/worktrees and file previews (capped at 256 KiB text /
  4 MiB raster image) **only inside known project roots**, save that device's
  own UI preferences, and revoke its own pairing.
- **Desktop-only (blocked remotely)**: browsing or reading outside known
  project roots, `projects.add` / `projects.remove`, `settings.update`,
  `agents.authenticate`, `workspace.openExternal`, `voice.install` /
  `voice.uninstall`, `remote.set` / `remote.createPairing`, `app.notify`, and
  `app.setBadge`. `settings.get` masks all agent environment variable values as
  `***`.

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub Security Advisories on
this repository (the **Security** tab → **Report a vulnerability**). Do not
open a public issue for a vulnerability.

Include the Vetro version, your OS, the agent(s) involved and steps to
reproduce. Remove tokens and personal paths from any logs you attach. The
project is volunteer-maintained; there is no bug bounty.

## Supported versions

Only the latest release receives security fixes.
