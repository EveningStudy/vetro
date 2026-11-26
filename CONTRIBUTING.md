# Contributing to Vetro

Thanks for helping out. Before changing the core, read `docs/ARCHITECTURE.md`;
before touching the UI protocol, read `docs/API.md`; before adding features or
changing the UI, read `docs/DESIGN.md`.

## Build and test

Prerequisites: a stable Rust toolchain (see `rust-toolchain.toml`), Node.js 24
and pnpm 10. On Linux also install the Tauri system packages listed in
`docs/desktop.md`.

```sh
pnpm --dir apps/web install --frozen-lockfile
pnpm --dir apps/web typecheck
pnpm --dir apps/web build
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

All of the above run in CI on Windows, macOS and Linux.

`crates/vetro-proto` is the wire contract: `cargo test -p vetro-proto`
regenerates its TypeScript bindings. If your change touches it, say so clearly
in the pull request.

## The product rules

Every change must keep the rules in `AGENTS.md`. In short:

1. **Zero injection** — what the user typed reaches the agent unchanged. No
   added prompts, instructions, tools, MCP servers, or settings.
2. **Zero footprint** — Vetro writes its own data only under `~/.vetro`, opens
   other agents' stores read-only, and never silently writes hidden files into
   workspaces.
3. **Native runtimes only** — run the official agent binaries installed on the
   machine. Never download an agent binary, never read or store credentials.
4. **No orphans** — every managed agent process dies when Vetro exits, and
   active background work is surfaced before release.
5. **Light (runtime and scope)** — one Rust process, small dependency tree, and
   a thin-client scope (not an IDE, terminal emulator, or workflow harness).
6. **Capability-driven, not privileged** — same UI rules and slot order for
   every agent; native protocol capabilities surface through unified slots.
7. **Progressive disclosure** — primary canvas focuses on user intent, pending
   actions, and final answers; secondary details stay collapsed or in drawers.

## Tests with live agents

The default test run uses the fake agents in
`crates/vetro-core/tests/fixtures` and never touches a real CLI. Tests that
launch real agents are marked `#[ignore]` and are run on purpose, never
automatically — each run spends the owner's agent subscription quota. When you
do run one, use the cheapest model, a one-word prompt and a scratch directory,
and never use permission-bypass flags.

## Commits

Small commits with an imperative subject line that says why the change exists.
No AI attribution in commit messages.
