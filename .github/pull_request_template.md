## What & why

<!-- one short paragraph -->

## Product rules

- [ ] Zero injection: user text reaches agents unchanged; no prompts, tools, MCP or settings added
- [ ] Zero footprint: writes stay under `~/.vetro`; other agents' stores opened read-only
- [ ] Native runtimes: no downloaded binaries, no credential handling
- [ ] No orphans: child processes die on exit (job object / process group)
- [ ] Light: no new heavy dependencies
- [ ] Same rules for every agent

## Verification

- [ ] `pnpm --dir apps/web typecheck` and `pnpm --dir apps/web build` pass
- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Changes under `crates/vetro-proto` (owned by the orchestrator) are flagged in the description above
