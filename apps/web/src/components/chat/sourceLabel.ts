import type { ThreadSummary } from "~/api/types";
import type { Translate } from "~/i18n";

const execCommands: Record<string, string> = {
  codex: "codex exec",
  claude: "claude -p",
  antigravity: "agy -p",
  devin: "devin -p",
};

const knownOrigins: Record<string, string> = {
  codex_exec: "codex exec",
  codex_cli_rs: "Codex CLI",
  codex_vscode: "Codex IDE",
  codex_cloud: "Codex Cloud",
  "claude-desktop": "Claude Desktop",
  "claude-vscode": "Claude Code IDE",
  "sdk-cli": "Claude Agent SDK",
  agy: "agy",
};

/** A short label for where a session was created, from the agent's own records. */
export function sourceLabel(thread: ThreadSummary, agentName: string | undefined, t: Translate) {
  const origin = thread.origin?.trim();
  if (origin && knownOrigins[origin]) return knownOrigins[origin];
  if (thread.source === "vetro") return "Vetro";
  if (thread.source === "exec") return execCommands[thread.agentId] ?? `${thread.agentId} exec`;
  if (origin && /[A-Z ]/.test(origin)) return origin;
  if (thread.source === "unknown") return agentName ?? thread.agentId;
  return `${agentName ?? thread.agentId} ${t(`source.${thread.source}`)}`;
}
