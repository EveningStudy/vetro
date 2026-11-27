# Antigravity

## How Verre connects

Verre starts the unmodified Antigravity CLI that is installed on the machine as an Agent Client Protocol server: `agy acp`, over stdio, in the project directory. The agent, its tools, its sign-in and its settings are Antigravity's own. Verre forwards what the user typed and renders what comes back. It adds no system prompt, no tools, no MCP servers and no replayed history, and it never reads or stores Antigravity credentials.

This is the integration Google documents for ACP clients ("Any ACP client can launch Antigravity CLI", <https://antigravity.google/docs/ide/extensions/acp/>) and the same path Zed, JetBrains IDEs and T3 Code use.

## Policy background (checked 2026-11-23)

Google's statements, newest first. Forum replies are from Google staff and describe how published policy applies; they are not legal advice, and Google can change the policy.

| Date | Source | What it says |
|---|---|---|
| 2026-11-21 | X, Patrick Toulme (Google DeepMind, Antigravity) | "You will NOT be banned ever for using Antigravity in T3Code." T3 Code uses the official ACP agent. A personal statement, backed by Theo (T3 Code) and Varun Mohan. |
| 2026-11-19..21 | Google AI Developers Forum, staff replies (threads 200310, 202874) | Human-driven sessions in a client are supported. Letting another coding agent or an orchestrator dispatch tasks to `agy` is not supported under consumer sign-in and can trigger automated restrictions. |
| 2026-11-12 | GitHub `google-antigravity/antigravity-cli` issue 1493, collaborator reply | Any interactive ACP v1 client, including custom non-editor UIs, may launch the unmodified `agy acp` binary when a human drives the session. Auto-approval modes inside such a session are fine. Not permitted on consumer OAuth: another agent, autonomous loop or background queue generating prompts for `agy acp` / `agy -p`, overriding Antigravity's system instructions, account rotation. |
| 2026-10-26, 2026-10-31 | Forum threads 192027 and 197131 (solved) | Running Google's official ACP agent inside an ACP-compatible client (T3 Code, Zed) with its native sign-in, for single-user interactive development, is supported and permitted. Token extraction, proxies and wrapping `agy -p` in third-party agent loops are enforced against. |
| ongoing | FAQ, <https://antigravity.google/docs/faq/> | "Antigravity CLI headless mode (`agy -p`) is intended for direct scripting and CI workflows. Wrapping `agy` inside third-party coding agents or applications, or automating account rotation, is not supported under consumer plans." |

## What this means in Verre

- Using Antigravity in Verre with a personal Google sign-in is the supported case: an interactive client, the official binary, the user's own prompts.
- Antigravity gets the same features as every other agent. Verre has no orchestration features for any agent, so there is nothing to switch off.
- Verre shows sessions that were started by another agent through a CLI (for example Claude running `agy -p`) like any other linked session; it only reads them. Whether to delegate work to Antigravity from another agent is the user's decision. Google's supported route for that is API-key mode (`"modelProvider": "gemini"` with `GEMINI_API_KEY`, or Google Cloud), which is outside the consumer-plan restriction. In Verre, an API key can be given to the Antigravity agent as an environment variable in Settings, and the agent's `gemini-api-key` sign-in method is offered next to the others.
- Outside first-party IDE integrations, the models Antigravity offers over ACP are decided by Google and may be limited to Gemini models.
