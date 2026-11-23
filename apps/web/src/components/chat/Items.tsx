import {
  BotIcon,
  BrainIcon,
  CheckIcon,
  CircleAlertIcon,
  CircleCheckIcon,
  CircleDashedIcon,
  CircleDotIcon,
  CopyIcon,
  ExternalLinkIcon,
  FileIcon,
  FilePenIcon,
  FileTextIcon,
  FolderInputIcon,
  GlobeIcon,
  InfoIcon,
  ListTodoIcon,
  PlugIcon,
  SearchIcon,
  ShieldQuestionIcon,
  SquareTerminalIcon,
  Trash2Icon,
  TriangleAlertIcon,
  WrenchIcon,
  XIcon,
} from "lucide-react";
import { memo, useEffect, useState } from "react";
import type { ItemOf, Question, ToolKind } from "~/api/types";
import { type Translate, useT } from "~/i18n";
import { clockTime, formatDuration, formatTokens } from "~/lib/format";
import { cn, copyText } from "~/lib/utils";
import { actions, useStore } from "~/state/store";
import { AgentIcon } from "../AgentIcon";
import { Button } from "../ui/button";
import { Tip } from "../ui/overlay";
import { Badge, Spinner } from "../ui/primitives";
import { Markdown } from "./Markdown";
import { sourceLabel } from "./sourceLabel";
import { Timeline } from "./Timeline";
import { DiffBlock, JsonBlock, TerminalBlock, TextBlock, diffStats } from "./ToolDetails";
import { WorkLogDetails, WorkLogRow } from "./WorkLog";

const toolIcons: Record<ToolKind, typeof WrenchIcon> = {
  read: FileTextIcon,
  edit: FilePenIcon,
  delete: Trash2Icon,
  move: FolderInputIcon,
  search: SearchIcon,
  execute: SquareTerminalIcon,
  think: BrainIcon,
  fetch: GlobeIcon,
  agent: BotIcon,
  mcp: PlugIcon,
  other: WrenchIcon,
};

function Timestamp({ ts }: { ts: number | null }) {
  if (!ts) return null;
  return (
    <span className="hidden shrink-0 text-2xs tabular-nums text-muted-foreground/70 opacity-0 transition-opacity group-hover/timeline-row:opacity-100 sm:inline">
      {clockTime(ts)}
    </span>
  );
}

export const UserMessage = memo(function UserMessage({ item }: { item: ItemOf<"user_message"> }) {
  const t = useT();
  const [copied, setCopied] = useState(false);
  const text = item.content.map((block) => (block.type === "text" ? block.text : "")).join("\n");
  const images = item.content.filter((block) => block.type === "image");
  const files = item.content.filter((block) => block.type === "file");
  return (
    <div className="group flex flex-col items-end gap-1">
      {images.length > 0 ? (
        <div className="flex max-w-[80%] flex-wrap justify-end gap-1.5">
          {images.map((image, index) =>
            image.type === "image" ? (
              <img
                key={index}
                alt={t("item.image")}
                src={image.data ? `data:${image.mimeType};base64,${image.data}` : (image.uri ?? undefined)}
                className="max-h-40 max-w-56 rounded-xl border object-cover"
              />
            ) : null,
          )}
        </div>
      ) : null}
      {text ? (
        <div className="min-w-0 max-w-[80%] rounded-2xl bg-message px-4 py-2.5 text-message-foreground">
          <div className="whitespace-pre-wrap break-words text-sm leading-relaxed">{text}</div>
          {files.length > 0 ? (
            <div className="mt-2 flex flex-wrap gap-1.5">
              {files.map((file, index) =>
                file.type === "file" ? (
                  <span key={index} className="inline-flex max-w-full items-center gap-1 rounded-md bg-background/60 px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
                    <FileIcon className="size-3 shrink-0" />
                    <span className="truncate">{file.path}</span>
                  </span>
                ) : null,
              )}
            </div>
          ) : null}
        </div>
      ) : null}
      <div className="flex items-center gap-2 pe-1 text-xs tabular-nums text-muted-foreground opacity-0 transition-opacity focus-within:opacity-100 group-hover:opacity-100 pointer-coarse:opacity-100">
        <span>{clockTime(item.ts)}</span>
        <button
          type="button"
          aria-label={t("common.copy")}
          className="inline-flex size-5 items-center justify-center rounded hover:bg-accent hover:text-foreground"
          onClick={() => {
            void copyText(text).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1200);
            });
          }}
        >
          {copied ? <CheckIcon className="size-3" /> : <CopyIcon className="size-3" />}
        </button>
      </div>
    </div>
  );
});

export const AgentMessage = memo(function AgentMessage({ item }: { item: ItemOf<"agent_message"> }) {
  return (
    <div className="min-w-0 px-0.5 text-sm leading-relaxed text-foreground">
      {item.text ? <Markdown text={item.text} className={item.streaming ? "streaming" : undefined} /> : null}
      {item.streaming && !item.text ? <span className="inline-block h-3.5 w-1.5 animate-pulse rounded-[1px] bg-foreground/55" /> : null}
    </div>
  );
});

export const Reasoning = memo(function Reasoning({ item }: { item: ItemOf<"reasoning"> }) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const preview = item.text.replace(/\*\*/g, "").replace(/\s+/g, " ").trim();
  return (
    <WorkLogRow
      icon={<BrainIcon className="size-4" />}
      expanded={open}
      onToggle={() => setOpen(!open)}
      label={
        <span className="flex min-w-0 items-baseline gap-1.5">
          <span className={cn("shrink-0 text-foreground/80", item.streaming && "live-tool-shine")}>
            {item.streaming ? t("item.thinking") : t("item.thought")}
          </span>
          {!open && preview ? <span className="min-w-0 truncate text-muted-foreground">{preview}</span> : null}
        </span>
      }
      trailing={<Timestamp ts={item.ts} />}
    >
      {open ? (
        <WorkLogDetails className="max-h-96 overflow-auto text-sm text-muted-foreground">
          <Markdown text={item.text} />
        </WorkLogDetails>
      ) : null}
    </WorkLogRow>
  );
});

const SHELL_WRAPPER =
  /^"?(?:[^"]*[\\/])?(?:pwsh|powershell|bash|sh|zsh|cmd)(?:\.exe)?"?\s+(?:-(?:NoProfile|NoLogo|NonInteractive)\s+)*(?:-Command|-c|-lc|\/c)\s+([\s\S]+)$/i;

/** Agents often run commands through a shell wrapper; show the command the user cares about. */
export function unwrapShell(command: string) {
  const match = SHELL_WRAPPER.exec(command.trim());
  if (!match) return command;
  const inner = match[1]!.trim();
  const quote = inner[0];
  return (quote === '"' || quote === "'") && inner.endsWith(quote) && inner.length > 1 ? inner.slice(1, -1) : inner;
}

function toolTitle(item: ItemOf<"tool_call">) {
  if (item.kind === "execute") {
    const lines = unwrapShell(item.title).split("\n").filter((line) => line.trim());
    return lines.length > 1 ? `${lines[0]} …` : (lines[0] ?? item.title);
  }
  const diffs = item.content.filter((block) => block.type === "diff");
  if (item.kind === "edit" && diffs.length > 0 && !/[./\\]/.test(item.title)) {
    const names = diffs.map((block) => (block as { path: string }).path.split(/[\\/]/).at(-1) ?? block.path);
    return `${item.title} · ${names.slice(0, 3).join(", ")}${names.length > 3 ? ` +${names.length - 3}` : ""}`;
  }
  return item.title;
}

function toolSummary(item: ItemOf<"tool_call">) {
  const stats = diffStats(item.content);
  if (stats.additions + stats.deletions === 0) return null;
  return (
    <span className="shrink-0 font-mono text-xs tabular-nums">
      <span className="text-diff-addition-foreground">+{stats.additions}</span>{" "}
      <span className="text-diff-deletion-foreground">−{stats.deletions}</span>
    </span>
  );
}

export const ToolCallRow = memo(function ToolCallRow({ item, expandChildren }: { item: ItemOf<"tool_call">; expandChildren?: boolean }) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const [showRaw, setShowRaw] = useState(false);
  const Icon = toolIcons[item.kind] ?? WrenchIcon;
  const running = item.status === "in_progress";
  const failed = item.status === "failed";
  const expandable = item.content.length > 0 || item.input !== null || item.output !== null;
  return (
    <div>
      <WorkLogRow
        icon={running ? <Spinner className="size-3.5" /> : <Icon className={cn("size-4", failed && "text-error")} />}
        expanded={open}
        onToggle={expandable ? () => setOpen(!open) : undefined}
        label={
          <span title={item.title} className={cn(failed ? "text-destructive-foreground" : "text-secondary-label", running && "live-tool-shine")}>
            {toolTitle(item)}
          </span>
        }
        trailing={
          <>
            {toolSummary(item)}
            {item.status === "pending" ? <span className="shrink-0 text-xs text-warning-foreground">{t("item.pending")}</span> : null}
            {failed ? <XIcon className="size-3 shrink-0 text-error" aria-label={t("item.failed")} /> : null}
            <Timestamp ts={item.ts} />
          </>
        }
      >
        {open ? (
          <WorkLogDetails>
            {item.content.map((block, index) => {
              switch (block.type) {
                case "terminal":
                  return <TerminalBlock key={index} command={block.command && unwrapShell(block.command)} output={block.output} exitCode={block.exitCode} running={running} />;
                case "diff":
                  return <DiffBlock key={index} block={block} />;
                case "text":
                  return <TextBlock key={index} text={block.text} />;
                case "image":
                  return <img key={index} alt="" src={`data:${block.mimeType};base64,${block.data}`} className="max-h-64 max-w-full rounded-lg border" />;
              }
            })}
            {item.input !== null || item.output !== null ? (
              showRaw ? (
                <>
                  {item.input !== null ? <JsonBlock label={`${t("item.input")}${item.name ? ` · ${item.name}` : ""}`} value={item.input} /> : null}
                  {item.output !== null ? <JsonBlock label={t("item.output")} value={item.output} /> : null}
                </>
              ) : (
                <button type="button" onClick={() => setShowRaw(true)} className="self-start text-xs text-muted-foreground hover:text-foreground hover:underline">
                  {t("item.raw")} ({item.name ?? item.kind})
                </button>
              )
            ) : null}
          </WorkLogDetails>
        ) : null}
      </WorkLogRow>
      {item.childThreadIds.map((childId) => (
        <ChildSession key={childId} threadId={childId} defaultOpen={expandChildren} />
      ))}
    </div>
  );
});

function ChildSession({ threadId, defaultOpen = false }: { threadId: string; defaultOpen?: boolean }) {
  const t = useT();
  const [open, setOpen] = useState(defaultOpen);
  const summary = useStore((s) => s.threads[threadId] ?? s.open[threadId]?.summary);
  const loaded = useStore((s) => !!s.open[threadId]);
  const agent = useStore((s) => s.agents.find((a) => a.id === (summary?.agentId ?? threadId.split(":")[0])));
  useEffect(() => {
    if (!open) return;
    actions.retain(threadId);
    return () => actions.release(threadId);
  }, [open, threadId]);

  const confidence = summary?.parent?.confidence;
  return (
    <div className="my-1 ms-7 overflow-hidden rounded-xl border bg-card/60 dark:bg-white/[0.015]">
      <div className="flex min-h-10 items-center gap-2 ps-3 pe-1.5">
        <button type="button" onClick={() => setOpen(!open)} className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 py-2 text-left" aria-expanded={open}>
          <AgentIcon agentId={summary?.agentId ?? threadId.split(":")[0]!} className="size-4" />
          <span className="shrink-0 text-sm font-medium">{t("item.childSession", { agent: agent?.name ?? summary?.agentId ?? "" })}</span>
          <span className="min-w-0 truncate text-sm text-muted-foreground">{summary?.title}</span>
          {summary ? <Badge variant="outline">{sourceLabel(summary, agent?.name, t)}</Badge> : null}
          {summary?.status === "running" ? <Spinner className="size-3.5 text-info-foreground" /> : null}
        </button>
        {confidence ? (
          <Tip label={t(confidence === "exact" ? "item.linkExact" : confidence === "prompt" ? "item.linkPrompt" : "item.linkHeuristic")}>
            <span className={cn("hidden size-1.5 shrink-0 rounded-full sm:block", confidence === "exact" ? "bg-success" : confidence === "prompt" ? "bg-info" : "bg-warning")} />
          </Tip>
        ) : null}
        <Button size="xs" variant="ghost-muted" onClick={() => actions.navigate({ view: "thread", threadId })}>
          {t("item.openSession")}
          <ExternalLinkIcon />
        </Button>
      </div>
      {open ? (
        <div className="max-h-[32rem] overflow-y-auto border-t px-3 py-3">
          {loaded ? <Timeline threadId={threadId} nested /> : <Spinner className="mx-auto my-4 block text-muted-foreground" />}
        </div>
      ) : null}
    </div>
  );
}

export const PlanCard = memo(function PlanCard({ item }: { item: ItemOf<"plan"> }) {
  const t = useT();
  const done = item.entries.filter((entry) => entry.status === "completed").length;
  return (
    <div className="rounded-xl border bg-card/60 px-3 py-2.5 dark:bg-white/[0.015]">
      <div className="mb-1.5 flex items-center gap-2 text-sm">
        <ListTodoIcon className="size-4 text-icon-muted" />
        <span className="font-medium">{t("item.plan")}</span>
        <span className="text-xs text-muted-foreground">{t("item.planProgress", { done, total: item.entries.length })}</span>
      </div>
      <ol className="flex flex-col gap-1">
        {item.entries.map((entry, index) => (
          <li key={index} className="flex items-start gap-2 text-sm leading-relaxed">
            {entry.status === "completed" ? (
              <CircleCheckIcon className="mt-1 size-3.5 shrink-0 text-success" />
            ) : entry.status === "in_progress" ? (
              <CircleDotIcon className="mt-1 size-3.5 shrink-0 text-info" />
            ) : (
              <CircleDashedIcon className="mt-1 size-3.5 shrink-0 text-muted-foreground" />
            )}
            <span className={cn(entry.status === "completed" ? "text-muted-foreground line-through decoration-muted-foreground/40" : entry.status === "in_progress" ? "font-medium" : "text-foreground/80")}>
              {entry.content}
            </span>
          </li>
        ))}
      </ol>
    </div>
  );
});

function resolutionLabel(item: ItemOf<"request">, t: Translate) {
  const resolution = item.resolution;
  if (!resolution) return null;
  if (resolution.type === "cancelled") return t("request.cancelled");
  if (resolution.type === "answered") {
    const answers = Object.values(resolution.answers).flat().join(", ");
    return answers ? `${t("request.answered")}: ${answers}` : t("request.answered");
  }
  const option = item.body.type === "permission" ? item.body.options.find((o) => o.id === resolution.optionId) : null;
  return t("request.chosen", { label: option?.label ?? resolution.optionId });
}

export const RequestItem = memo(function RequestItem({ item, threadId }: { item: ItemOf<"request">; threadId: string }) {
  const t = useT();
  if (item.resolution) {
    const rejected =
      item.resolution.type === "cancelled" ||
      (item.resolution.type === "selected" &&
        item.body.type === "permission" &&
        item.body.options.find((o) => o.id === (item.resolution as { optionId: string }).optionId)?.kind.startsWith("reject"));
    return (
      <WorkLogRow
        icon={<ShieldQuestionIcon className="size-4" />}
        label={
          <span className="flex min-w-0 items-baseline gap-1.5">
            <span className="min-w-0 truncate">{item.title}</span>
            <span className={cn("shrink-0", rejected ? "text-destructive-foreground" : "text-foreground")}>{resolutionLabel(item, t)}</span>
          </span>
        }
        trailing={<Timestamp ts={item.ts} />}
      />
    );
  }
  return (
    <div className="overflow-hidden rounded-xl border border-warning/40 bg-warning-surface/60 shadow-composer">
      <div className="flex items-start gap-2.5 px-3.5 pt-3">
        <ShieldQuestionIcon className="mt-0.5 size-4 shrink-0 text-warning-foreground" />
        <div className="min-w-0 flex-1">
          <div className="text-xs font-medium text-warning-foreground">{t("request.waiting")}</div>
          <div className="mt-0.5 text-sm font-medium text-foreground">{item.title}</div>
          {item.detail ? (
            <pre className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap break-words rounded-lg border bg-background/70 px-2.5 py-1.5 font-mono text-xs text-foreground/90">
              {item.detail}
            </pre>
          ) : null}
        </div>
      </div>
      {item.body.type === "permission" ? (
        <div className="flex flex-wrap gap-2 px-3.5 pt-3 pb-3 ps-10">
          {item.body.options.map((option, index) => (
            <Button
              key={option.id}
              size="sm"
              variant={option.kind.startsWith("reject") ? "outline" : index === 0 ? "default" : "outline"}
              onClick={() => void actions.respond(threadId, item.requestId, { type: "selected", optionId: option.id })}
            >
              {option.label}
            </Button>
          ))}
        </div>
      ) : (
        <QuestionsForm item={item} questions={item.body.questions} threadId={threadId} />
      )}
    </div>
  );
});

function QuestionsForm({ item, questions, threadId }: { item: ItemOf<"request">; questions: Question[]; threadId: string }) {
  const t = useT();
  const [answers, setAnswers] = useState<Record<string, string[]>>({});
  const [free, setFree] = useState<Record<string, string>>({});
  const complete = questions.every((q) => (answers[q.id]?.length ?? 0) > 0 || (free[q.id] ?? "").trim());
  const toggle = (question: Question, label: string) => {
    const current = answers[question.id] ?? [];
    const next = question.multiSelect ? (current.includes(label) ? current.filter((l) => l !== label) : [...current, label]) : [label];
    setAnswers({ ...answers, [question.id]: next });
  };
  return (
    <form
      className="flex flex-col gap-4 px-3.5 pt-3 pb-3 ps-10"
      onSubmit={(event) => {
        event.preventDefault();
        const result: Record<string, string[]> = {};
        for (const question of questions) {
          const text = (free[question.id] ?? "").trim();
          result[question.id] = [...(answers[question.id] ?? []), ...(text ? [text] : [])];
        }
        void actions.respond(threadId, item.requestId, { type: "answered", answers: result });
      }}
    >
      {questions.map((question) => (
        <fieldset key={question.id} className="flex flex-col gap-1.5">
          <legend className="mb-1.5 text-sm">
            {question.header ? <span className="me-1.5 text-xs font-medium uppercase tracking-wide text-muted-foreground">{question.header}</span> : null}
            <span className="text-foreground">{question.text}</span>
          </legend>
          <div className="flex flex-wrap gap-1.5">
            {question.choices.map((choice) => {
              const selected = answers[question.id]?.includes(choice.label) ?? false;
              return (
                <button
                  key={choice.label}
                  type="button"
                  aria-pressed={selected}
                  onClick={() => toggle(question, choice.label)}
                  className={cn(
                    "flex min-w-0 flex-col items-start rounded-lg border px-2.5 py-1.5 text-left text-sm transition-colors",
                    selected ? "border-primary/60 bg-primary/8 text-foreground ring-1 ring-primary/30" : "border-input bg-background/70 hover:bg-accent",
                  )}
                >
                  <span className="flex items-center gap-1.5 font-medium">
                    {question.multiSelect ? (
                      <span className={cn("flex size-3.5 items-center justify-center rounded-[3px] border", selected && "border-primary bg-primary text-primary-foreground")}>
                        {selected ? <CheckIcon className="size-2.5" /> : null}
                      </span>
                    ) : null}
                    {choice.label}
                  </span>
                  {choice.description ? <span className="text-xs text-muted-foreground">{choice.description}</span> : null}
                </button>
              );
            })}
          </div>
          {question.allowFreeText ? (
            <input
              value={free[question.id] ?? ""}
              onChange={(event) => setFree({ ...free, [question.id]: event.target.value })}
              placeholder={t("request.otherPlaceholder")}
              className="h-8 w-full max-w-sm rounded-md border border-input bg-background/70 px-2.5 text-sm outline-none placeholder:text-placeholder focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/24"
            />
          ) : null}
        </fieldset>
      ))}
      <div className="flex gap-2">
        <Button size="sm" type="submit" disabled={!complete}>
          {t("request.submit")}
        </Button>
        <Button size="sm" variant="ghost-muted" onClick={() => void actions.respond(threadId, item.requestId, { type: "cancelled" })}>
          {t("request.dismiss")}
        </Button>
      </div>
    </form>
  );
}

export const NoticeRow = memo(function NoticeRow({ item }: { item: ItemOf<"notice"> }) {
  const Icon = item.level === "error" ? CircleAlertIcon : item.level === "warning" ? TriangleAlertIcon : InfoIcon;
  return (
    <WorkLogRow
      icon={<Icon className={cn("size-4", item.level === "error" ? "text-error" : item.level === "warning" ? "text-warning" : "text-info")} />}
      label={
        <span className={cn(item.level === "error" ? "text-error-foreground" : item.level === "warning" ? "text-warning-foreground" : "text-secondary-label")}>
          {item.text}
        </span>
      }
      trailing={<Timestamp ts={item.ts} />}
    />
  );
});

export const TurnEnd = memo(function TurnEnd({ item }: { item: ItemOf<"turn_end"> }) {
  const t = useT();
  const parts: string[] = [];
  if (item.stopReason !== "end_turn") parts.push(t(`item.stop.${item.stopReason}`));
  if (item.durationMs != null) parts.push(t("item.turnEnd", { duration: formatDuration(item.durationMs) }));
  if (item.usage && (item.usage.input != null || item.usage.output != null))
    parts.push(t("item.turnTokens", { input: formatTokens(item.usage.input), output: formatTokens(item.usage.output) }));
  if (parts.length === 0) return null;
  return (
    <div className={cn("flex items-center gap-3 py-1 text-xs tabular-nums", item.stopReason === "error" ? "text-error-foreground" : "text-muted-foreground")}>
      <span className="h-px flex-1 bg-border" />
      <span>{parts.join(" · ")}</span>
      <span className="h-px flex-1 bg-border" />
    </div>
  );
});
