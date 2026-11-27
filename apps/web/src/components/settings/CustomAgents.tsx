import { PencilIcon, PlusIcon, Trash2Icon } from "lucide-react";
import { useState } from "react";
import type { CustomAcpAgent, Settings } from "~/api/types";
import { useT } from "~/i18n";
import { Button } from "../ui/button";
import { Badge, Input, Label, Textarea } from "../ui/primitives";

/** Agents the core ships adapters for. A custom entry with one of these ids is ignored by the core. */
export const BUILTIN_AGENT_IDS = [
  "claude",
  "codex",
  "antigravity",
  "devin",
  "kiro",
  "gemini",
  "opencode",
  "cursor",
  "copilot",
  "goose",
  "cline",
  "kimi",
  "qwen-code",
  "droid",
  "amp",
  "auggie",
  "vibe",
  "kilo",
  "grok",
  "deepseek",
  "codebuddy",
];

const EMPTY: CustomAcpAgent = { id: "", name: "", command: "", args: [], env: {} };

function slug(name: string) {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

function envText(env: Record<string, string>) {
  return Object.entries(env)
    .map(([key, value]) => `${key}=${value}`)
    .join("\n");
}

function parseEnv(text: string) {
  const env: Record<string, string> = {};
  for (const line of text.split("\n")) {
    const index = line.indexOf("=");
    if (index > 0) env[line.slice(0, index).trim()] = line.slice(index + 1);
  }
  return env;
}

/** User-defined ACP agents: any command that speaks ACP on stdio. Saved as the whole list. */
export function CustomAgents({
  settings,
  readOnly,
  onSave,
  reservedIds,
}: {
  settings: Settings | null;
  readOnly: boolean;
  onSave: (patch: unknown) => Promise<Settings | null>;
  reservedIds: string[];
}) {
  const t = useT();
  const [editing, setEditing] = useState<{ original: string | null; draft: CustomAcpAgent; args: string; env: string } | null>(null);
  const list = settings?.customAcpAgents ?? [];

  const save = async () => {
    if (!editing) return;
    const id = editing.draft.id || slug(editing.draft.name);
    const entry: CustomAcpAgent = {
      id,
      name: editing.draft.name.trim(),
      command: editing.draft.command.trim(),
      args: editing.args.split("\n").map((arg) => arg.trim()).filter(Boolean),
      env: parseEnv(editing.env),
    };
    const next = editing.original === null ? [...list, entry] : list.map((agent) => (agent.id === editing.original ? entry : agent));
    if (await onSave({ customAcpAgents: next })) setEditing(null);
  };

  const id = editing ? editing.draft.id || slug(editing.draft.name) : "";
  const clash = !!editing && id !== editing.original && (reservedIds.includes(id) || list.some((agent) => agent.id === id));
  const valid = !!editing && !!editing.draft.name.trim() && !!editing.draft.command.trim() && !!id && !clash;

  return (
    <div className="mt-6">
      <div className="mb-3 flex items-center justify-between gap-3">
        <div>
          <h3 className="text-sm font-semibold">{t("custom.title")}</h3>
          <p className="text-xs text-muted-foreground">{t("custom.hint")}</p>
        </div>
        {editing || readOnly ? null : (
          <Button size="sm" variant="outline" onClick={() => setEditing({ original: null, draft: { ...EMPTY }, args: "acp", env: "" })}>
            <PlusIcon />
            {t("custom.add")}
          </Button>
        )}
      </div>
      <div className="divide-y overflow-hidden rounded-xl border bg-card/70 dark:bg-white/[0.015]">
        {list.length === 0 && !editing ? <div className="px-4 py-3 text-sm text-muted-foreground">{t("custom.none")}</div> : null}
        {list.map((agent) =>
          editing?.original === agent.id ? null : (
            <div key={agent.id} className="flex items-center gap-3 px-4 py-3">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 text-sm font-medium">
                  {agent.name}
                  {BUILTIN_AGENT_IDS.includes(agent.id) ? <Badge variant="warning">{t("custom.shadowed")}</Badge> : null}
                </div>
                <div className="truncate font-mono text-[11px] text-muted-foreground">{[agent.command, ...agent.args].join(" ")}</div>
              </div>
              {readOnly ? null : (
                <>
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    aria-label={t("thread.rename")}
                    onClick={() => setEditing({ original: agent.id, draft: { ...agent }, args: agent.args.join("\n"), env: envText(agent.env) })}
                  >
                    <PencilIcon />
                  </Button>
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    aria-label={t("custom.remove")}
                    onClick={() => void onSave({ customAcpAgents: list.filter((entry) => entry.id !== agent.id) })}
                  >
                    <Trash2Icon />
                  </Button>
                </>
              )}
            </div>
          ),
        )}
        {editing ? (
          <form
            className="flex flex-col gap-3 px-4 py-4"
            onSubmit={(event) => {
              event.preventDefault();
              if (valid) void save();
            }}
          >
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="custom-name">{t("custom.name")}</Label>
                <Input
                  id="custom-name"
                  autoFocus
                  value={editing.draft.name}
                  onChange={(event) => setEditing({ ...editing, draft: { ...editing.draft, name: event.target.value } })}
                />
                <span className="font-mono text-[11px] text-muted-foreground">
                  id: {id || "—"}
                  {clash ? ` · ${t("custom.idTaken")}` : ""}
                </span>
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="custom-command">{t("custom.command")}</Label>
                <Input
                  id="custom-command"
                  value={editing.draft.command}
                  placeholder="my-agent"
                  onChange={(event) => setEditing({ ...editing, draft: { ...editing.draft, command: event.target.value } })}
                  className="font-mono text-xs sm:text-xs"
                />
              </div>
            </div>
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="custom-args">{t("settings.extraArgs")}</Label>
                <Textarea id="custom-args" rows={3} value={editing.args} onChange={(event) => setEditing({ ...editing, args: event.target.value })} />
                <span className="text-xs text-muted-foreground">{t("settings.extraArgsHint")}</span>
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="custom-env">{t("settings.env")}</Label>
                <Textarea id="custom-env" rows={3} value={editing.env} onChange={(event) => setEditing({ ...editing, env: event.target.value })} />
                <span className="text-xs text-muted-foreground">{t("settings.envHint")}</span>
              </div>
            </div>
            <div className="flex justify-end gap-2">
              <Button variant="ghost" onClick={() => setEditing(null)}>
                {t("common.cancel")}
              </Button>
              <Button type="submit" disabled={!valid}>
                {t("common.save")}
              </Button>
            </div>
          </form>
        ) : null}
      </div>
    </div>
  );
}
