# Security policy

## What Vetro can access

Vetro is a local process. It starts the coding-agent CLIs installed on your
machine (Claude Code, Codex, Antigravity, Devin) as child processes with your
own permissions, and it serves the UI on `127.0.0.1` only. Everything it writes
goes under `~/.vetro`; the agents' own session stores are opened read-only.

Vetro never sees agent credentials: sign-in happens inside each agent's own
flow. Remote access is exclusively the user's own Tailscale network (via
`tailscale serve` or a reverse proxy the user runs) — Vetro never binds a
public interface, and every request still requires the local token or a
paired-device session cookie.

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub Security Advisories on
this repository (the **Security** tab → **Report a vulnerability**). Do not
open a public issue for a vulnerability.

Include the Vetro version, your OS, the agent(s) involved and steps to
reproduce. Remove tokens and personal paths from any logs you attach. The
project is volunteer-maintained; there is no bug bounty.

## Supported versions

Only the latest release receives security fixes.
