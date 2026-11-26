# Vetro design & anti-bloat guide

Vetro is a thin, calm, glass-like window over native coding agents. Features are easy to add one by one; coherence is lost one shortcut at a time. Read this before adding a feature, control, or setting.

## Decision hierarchy

When two goals pull in opposite directions, resolve them in this strict order:

1. **Vetro's product philosophy (`AGENTS.md`):** zero injection, zero footprint (strict read-only on other tools' stores; no silent workspace files), native runtimes only, no orphans, light in both runtime and scope, capability-driven equality across agents, and progressive disclosure. Nothing overrides this.
2. **T3 Code as the concrete UI/UX baseline:** for how controls look, where buttons live, how layouts respond to width, and how settings are grouped, follow T3 Code rather than inventing ad-hoc patterns.
3. **The specific feature request:** fit it cleanly into (1) and (2). If it cannot fit cleanly, stop and propose a simpler shape before writing code.

## Scope guardrails & priority

- **Core experience always wins over side features.** Polishing conversation flow, composer ergonomics, approval interactions, history reliability, and agent stability always takes priority over adding peripheral features.
- **Explicit non-goals:** Vetro does not become a code editor, built-in terminal emulator, embedded browser, or multi-agent workflow/schedule orchestrator. Keep editing and shell work delegated to the user's external tools.
- **T3 Code is a UI reference, not a feature checklist.** T3 Code ships 25+ settings pages and harness features (scheduled tasks, workflows, secrets, cloud, labs, MCP). Adopt its visual restraint and layout grammar; reject its harness scope creep.

## Using the T3 Code reference

A read-only checkout lives at `.orchestra/ref/t3code` during development (not shipped in the repository). Check it before designing UI changes:

- Layout, shell, and sidebar: `.orchestra/ref/t3code/apps/web/src/components/sidebar/` and `AppSidebarLayout.tsx`
- Chat timeline, composer, and option pickers: `.orchestra/ref/t3code/apps/web/src/components/chat/` and `components/permissions/`
- Settings grouping and rows: `.orchestra/ref/t3code/apps/web/src/components/settings/` and `routes/_chat.settings.*.tsx`
- Diffs, previews, and UI primitives: `.orchestra/ref/t3code/apps/web/src/components/diffs/` and `components/ui/`

### What to adopt vs. what to reject

- **Adopt:** visual restraint, spacing, typography, component ergonomics, muted treatment of secondary/danger indicators, responsive collapse behavior, clear domain grouping in Settings.
- **Reject:** anything tied to T3 Code's agent harness role — prompt or MCP injection, bundled orchestration workflows, scheduled tasks/cloud harness tabs, Electron/Node weight, agent-specific UI favoritism, or copying T3 Code's name, trademarks, and icons.
- **Vetro-only features** (cross-agent history, parent/child session links, raw wire log, offline voice, private remote pairing) must reuse T3 Code's visual grammar and progressive-disclosure layering instead of inventing a new style.

## Anti-bloat rules by surface

### Primary chat & composer

- The conversation canvas belongs to user intent, pending human actions, and final answers. Secondary details (reasoning, tool logs, diffs, raw wire traffic, child threads) stay collapsed or in on-demand drawers by default.
- The composer footer has a strict visual budget. Keep option slots in a fixed order across all agents (`agent -> model -> thinking -> permissions/mode -> overflow`) so muscle memory never breaks when switching agents.
- Every control added to the composer must degrade gracefully at narrow widths (wrap, truncate labels cleanly, or fold into an overflow menu) and never overlap neighboring buttons.

### Settings information architecture

- **Prefer defaults over switches.** Before adding a setting, ask whether a better default eliminates the need for a toggle.
- **Never treat "General" as a junk drawer.** Group settings by clear user domain (`General`, `Threads`, `Appearance`, `Notifications`, `Agents`, `Voice`, `Remote`, `About`).
- **Split proactively:** when a sub-domain grows past ~2 related controls (for example notifications/badges/sounds) or a section grows beyond ~6 rows, give it its own section rather than stacking unrelated rows together.
- **Scale agent lists cleanly:** when many agents are supported, highlight installed/enabled agents first and fold uninstalled agents behind an expandable section.

### Whole-app sanity check (before implementing)

Before adding a feature, control, or setting, verify all four:

1. Does it respect Vetro's product rules without expanding scope into an IDE, terminal, or agent harness?
2. How does T3 Code place and style the closest equivalent? If Vetro differs, is the reason justified by Vetro's philosophy?
3. Does the whole surrounding screen still feel calm and balanced after the change, or should adjacent items be regrouped/simplified at the same time?
4. Could this be done with less UI, fewer states, or a cleaner default?

When a surface accumulates friction from several small additions, schedule a dedicated **subtraction / cleanup pass** (no new features — only regrouping, simplifying controls, and aligning with T3 Code).
