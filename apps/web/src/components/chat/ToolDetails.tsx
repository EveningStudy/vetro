import { useMemo } from "react";
import type { JsonValue, ToolContent } from "~/api/types";
import { useT } from "~/i18n";
import { cn } from "~/lib/utils";
import { relativeToCwd } from "~/lib/format";
import { OpenFileButton } from "../OpenFileButton";
import { useHighlighter } from "./highlight";
import { useThreadCwd } from "./threadContext";

const panel = "min-w-0 overflow-hidden rounded-lg border bg-muted/60 dark:bg-white/[0.02]";
const mono = "font-mono text-xs leading-relaxed";

export function TerminalBlock({ command, output, exitCode, running }: { command: string | null; output: string; exitCode: number | null; running?: boolean }) {
  const t = useT();
  return (
    <div className={panel}>
      {command ? (
        <div className={cn(mono, "flex items-start gap-2 border-b px-3 py-1.5 text-foreground")}>
          <span className="select-none text-muted-foreground">$</span>
          <span className="min-w-0 flex-1 whitespace-pre-wrap break-all">{command}</span>
          {exitCode !== null ? (
            <span className={cn("shrink-0 text-2xs", exitCode === 0 ? "text-muted-foreground" : "text-error-foreground")}>
              {t("item.exitCode", { code: exitCode })}
            </span>
          ) : null}
        </div>
      ) : null}
      {output || running ? (
        <pre className={cn(mono, "max-h-72 overflow-auto whitespace-pre-wrap break-words px-3 py-2 text-secondary-label")}>
          {output}
          {running ? <span className="inline-block h-3 w-1.5 translate-y-0.5 animate-pulse bg-muted-foreground/60" /> : null}
        </pre>
      ) : null}
    </div>
  );
}

type DiffLine = { kind: "add" | "del" | "ctx" | "hunk"; text: string };

function parseUnified(unified: string): DiffLine[] {
  if (!unified.split("\n").some((line) => line.startsWith("@@"))) return unified.split("\n").map((text) => ({ kind: "add", text }));
  return unified
    .split("\n")
    .filter((line) => !line.startsWith("--- ") && !line.startsWith("+++ ") && !line.startsWith("diff --git") && !line.startsWith("index "))
    .map((line) => {
      if (line.startsWith("@@")) return { kind: "hunk", text: line };
      if (line.startsWith("+")) return { kind: "add", text: line.slice(1) };
      if (line.startsWith("-")) return { kind: "del", text: line.slice(1) };
      return { kind: "ctx", text: line.startsWith(" ") ? line.slice(1) : line };
    });
}

function lineDiff(oldText: string, newText: string): DiffLine[] {
  const a = oldText.split("\n");
  const b = newText.split("\n");
  if (a.length * b.length > 4_000_000) {
    return [...a.map((text) => ({ kind: "del" as const, text })), ...b.map((text) => ({ kind: "add" as const, text }))];
  }
  const dp: number[][] = Array.from({ length: a.length + 1 }, () => new Array<number>(b.length + 1).fill(0));
  for (let i = a.length - 1; i >= 0; i--)
    for (let j = b.length - 1; j >= 0; j--) dp[i]![j] = a[i] === b[j] ? dp[i + 1]![j + 1]! + 1 : Math.max(dp[i + 1]![j]!, dp[i]![j + 1]!);
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      out.push({ kind: "ctx", text: a[i]! });
      i++;
      j++;
    } else if (dp[i + 1]![j]! >= dp[i]![j + 1]!) out.push({ kind: "del", text: a[i++]! });
    else out.push({ kind: "add", text: b[j++]! });
  }
  while (i < a.length) out.push({ kind: "del", text: a[i++]! });
  while (j < b.length) out.push({ kind: "add", text: b[j++]! });
  return out;
}

export function diffStats(content: ToolContent[]) {
  let additions = 0;
  let deletions = 0;
  for (const block of content) {
    if (block.type !== "diff") continue;
    for (const line of diffLines(block)) {
      if (line.kind === "add") additions++;
      if (line.kind === "del") deletions++;
    }
  }
  return { additions, deletions };
}

function diffLines(block: Extract<ToolContent, { type: "diff" }>) {
  if (block.unified) return parseUnified(block.unified);
  if (block.oldText === null) return (block.newText ?? "").split("\n").map((text) => ({ kind: "add" as const, text }));
  return lineDiff(block.oldText, block.newText ?? "");
}

function languageOf(path: string) {
  return path.split(".").at(-1);
}

export function DiffBlock({ block, bare }: { block: Extract<ToolContent, { type: "diff" }>; bare?: boolean }) {
  const t = useT();
  const highlight = useHighlighter();
  const cwd = useThreadCwd();
  const lines = useMemo(() => diffLines(block), [block]);
  const lang = languageOf(block.path);
  const additions = lines.filter((line) => line.kind === "add").length;
  const deletions = lines.filter((line) => line.kind === "del").length;
  return (
    <div className={bare ? "min-w-0 overflow-hidden" : panel}>
      {bare ? null : (
      <div className="group/diff-header flex items-center gap-2 border-b px-3 py-1.5 text-xs">
        <span className="min-w-0 flex-1 truncate font-mono text-foreground" title={block.path}>
          {relativeToCwd(block.path, cwd)}
        </span>
        <OpenFileButton cwd={cwd} path={block.path} className="opacity-0 group-hover/diff-header:opacity-100 focus-visible:opacity-100 pointer-coarse:opacity-100" />
        {block.oldText === null && !block.unified ? <span className="text-muted-foreground">{t("item.newFile")}</span> : null}
        <span className="shrink-0 font-mono tabular-nums">
          <span className="text-diff-addition-foreground">+{additions}</span>{" "}
          <span className="text-diff-deletion-foreground">−{deletions}</span>
        </span>
      </div>
      )}
      <div className={cn(mono, "max-h-96 overflow-auto py-1")}>
        {lines.map((line, index) => {
          const html = line.kind === "hunk" || !highlight ? null : highlight(line.text, lang);
          return (
            <div
              key={index}
              className={cn(
                "flex min-w-max",
                line.kind === "add" && "bg-diff-addition",
                line.kind === "del" && "bg-diff-deletion",
                line.kind === "hunk" && "my-0.5 bg-info/6 text-info-foreground",
              )}
            >
              <span
                className={cn(
                  "w-6 shrink-0 select-none text-center",
                  line.kind === "add" ? "text-diff-addition-foreground" : line.kind === "del" ? "text-diff-deletion-foreground" : "text-muted-foreground/50",
                )}
              >
                {line.kind === "add" ? "+" : line.kind === "del" ? "−" : ""}
              </span>
              {html ? (
                <span className="whitespace-pre pe-4" dangerouslySetInnerHTML={{ __html: html || " " }} />
              ) : (
                <span className="whitespace-pre pe-4">{line.text || " "}</span>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

export function JsonBlock({ label, value }: { label: string; value: JsonValue }) {
  const text = typeof value === "string" ? value : JSON.stringify(value, null, 2);
  const highlight = useHighlighter();
  const html = useMemo(() => (typeof value === "string" || !highlight ? null : highlight(text, "json")), [highlight, text, value]);
  return (
    <div className={panel}>
      <div className="border-b px-3 py-1 text-2xs font-medium uppercase tracking-wide text-muted-foreground">{label}</div>
      {html ? (
        <pre className={cn(mono, "max-h-64 overflow-auto px-3 py-2")} dangerouslySetInnerHTML={{ __html: html }} />
      ) : (
        <pre className={cn(mono, "max-h-64 overflow-auto whitespace-pre-wrap break-words px-3 py-2 text-secondary-label")}>{text}</pre>
      )}
    </div>
  );
}

export function TextBlock({ text }: { text: string }) {
  return <pre className={cn(mono, "max-h-64 overflow-auto whitespace-pre-wrap break-words text-secondary-label")}>{text}</pre>;
}
