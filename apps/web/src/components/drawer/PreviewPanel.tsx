import { CodeIcon, CopyIcon, FileIcon, FolderOpenIcon, RefreshCwIcon, XIcon } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { client } from "~/api/client";
import type { WorkspaceFileResult } from "~/api/types";
import { useT } from "~/i18n";
import { basename, formatBytes } from "~/lib/format";
import { cn, copyText } from "~/lib/utils";
import { actions, useIsLocalDesktop } from "~/state/store";
import { errorMessage, toast } from "~/state/toast";
import { useHighlighter } from "../chat/highlight";
import { Markdown } from "../chat/Markdown";
import { Button } from "../ui/button";
import { Tip } from "../ui/overlay";
import { Spinner } from "../ui/primitives";
import { Segmented } from "../ui/segmented";
import { useHeaderChromeClass, useWindowDragProps } from "../WindowChrome";

const EXECUTABLE = /\.(?:exe|msi|dll|dmg|appimage|so|dylib|pkg|deb|rpm|iso)$/i;
const PER_LINE_LIMIT = 4000;

type LoadState = { loading: true } | { loading: false; result: WorkspaceFileResult; error: null } | { loading: false; result: null; error: string };

/** Splits highlight.js output into lines, closing and reopening spans that cross a line break. */
function splitHighlighted(html: string) {
  const lines: string[] = [];
  const open: string[] = [];
  let current = "";
  let last = 0;
  for (const match of html.matchAll(/<span[^>]*>|<\/span>|\n/g)) {
    const index = match.index ?? 0;
    current += html.slice(last, index);
    last = index + match[0].length;
    if (match[0] === "\n") {
      lines.push(current + "</span>".repeat(open.length));
      current = open.join("");
    } else if (match[0] === "</span>") {
      open.pop();
      current += match[0];
    } else {
      open.push(match[0]);
      current += match[0];
    }
  }
  lines.push(current + html.slice(last));
  return lines;
}

function dropTrailingEmpty(lines: string[]) {
  return lines.length > 1 && lines.at(-1) === "" ? lines.slice(0, -1) : lines;
}

function SourceView({ text, language, line }: { text: string; language: string | undefined; line?: number }) {
  const highlight = useHighlighter();
  const targetRef = useRef<HTMLDivElement>(null);
  const plain = useMemo(() => dropTrailingEmpty(text.split("\n")), [text]);
  const html = useMemo(() => {
    if (!highlight || !language) return null;
    const whole = highlight(text, language);
    if (whole !== null) {
      const lines = dropTrailingEmpty(splitHighlighted(whole));
      if (lines.length === plain.length) return lines;
    }
    if (plain.length > PER_LINE_LIMIT) return null;
    const perLine = plain.map((source) => highlight(source, language));
    return perLine.every((entry) => entry !== null) ? (perLine as string[]) : null;
  }, [highlight, text, language, plain]);

  useEffect(() => {
    if (line) targetRef.current?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [line, text]);

  const gutter = `${String(plain.length).length + 1}ch`;
  return (
    <div className="min-w-max py-2 font-mono text-xs leading-5">
      {plain.map((source, index) => {
        const number = index + 1;
        const target = number === line;
        const lineHtml = html?.[index];
        return (
          <div
            key={index}
            ref={target ? targetRef : undefined}
            className={cn("flex border-l-2 border-transparent", target && "border-info bg-info/15")}
          >
            <span style={{ width: gutter }} className="shrink-0 select-none ps-2 pe-3 text-right tabular-nums text-muted-foreground/60 box-content">
              {number}
            </span>
            {lineHtml !== undefined ? (
              <span className="whitespace-pre pe-4" dangerouslySetInnerHTML={{ __html: lineHtml || " " }} />
            ) : (
              <span className="whitespace-pre pe-4">{source || " "}</span>
            )}
          </div>
        );
      })}
    </div>
  );
}

/** Read-only preview of one workspace file: source with line numbers, rendered Markdown or an image. */
export function PreviewPanel({ cwd, path, line }: { cwd: string; path: string; line?: number }) {
  const t = useT();
  const drag = useWindowDragProps();
  const chromeClass = useHeaderChromeClass("drawer");
  const local = useIsLocalDesktop();
  const [state, setState] = useState<LoadState>({ loading: true });
  const [attempt, setAttempt] = useState(0);
  const [mode, setMode] = useState<"preview" | "source">("preview");

  useEffect(() => {
    let cancelled = false;
    setState({ loading: true });
    client()
      .request("workspace.readFile", { cwd, path })
      .then((result) => !cancelled && setState({ loading: false, result, error: null }))
      .catch((error: unknown) => !cancelled && setState({ loading: false, result: null, error: errorMessage(error) }));
    return () => {
      cancelled = true;
    };
  }, [cwd, path, attempt]);

  const result = state.loading ? null : state.result;
  const content = result?.content;
  const isMarkdown = content?.kind === "text" && content.language === "markdown";
  const name = basename(path) || path;
  const copyPath = () => void copyText(result?.resolvedPath ?? path).then((ok) => ok && toast.success(t("common.copied")));
  const language = content?.kind === "text" ? (content.language ?? path.split(".").at(-1)) : undefined;

  useEffect(() => {
    if (isMarkdown && line) setMode("source");
  }, [isMarkdown, line]);

  return (
    <>
      <div {...drag} className={cn("flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 border-b ps-4 pe-2", chromeClass)}>
        <FileIcon className="size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 truncate text-sm font-medium" title={result?.resolvedPath ?? path}>
          {name}
        </span>
        {result ? <span className="shrink-0 rounded-md bg-muted px-1.5 py-0.5 text-2xs tabular-nums text-muted-foreground">{formatBytes(result.sizeBytes)}</span> : null}
        <span className="flex-1" />
        {isMarkdown ? (
          <Segmented
            value={mode}
            onChange={setMode}
            label={t("preview.mode")}
            className="shrink-0 [&>button]:h-6 [&>button]:px-2"
            options={[
              { value: "preview", label: t("preview.rendered") },
              { value: "source", label: t("preview.source") },
            ]}
          />
        ) : null}
        {local ? (
          <Tip label={t("open.reveal")}>
            <Button size="icon-sm" variant="ghost" aria-label={t("open.reveal")} onClick={() => void actions.openExternal(cwd, "folder", path)}>
              <FolderOpenIcon />
            </Button>
          </Tip>
        ) : null}
        {local && !EXECUTABLE.test(path) ? (
          <Tip label={t("open.editor")}>
            <Button size="icon-sm" variant="ghost" aria-label={t("open.editor")} onClick={() => void actions.openExternal(cwd, "editor", path, line)}>
              <CodeIcon />
            </Button>
          </Tip>
        ) : null}
        <Tip label={t("open.copyFilePath")}>
          <Button size="icon-sm" variant="ghost" aria-label={t("open.copyFilePath")} onClick={copyPath}>
            <CopyIcon />
          </Button>
        </Tip>
        <Tip label={t("common.close")}>
          <Button size="icon-sm" variant="ghost" aria-label={t("common.close")} onClick={actions.closeDrawer}>
            <XIcon />
          </Button>
        </Tip>
      </div>
      <div className="min-h-0 flex-1 overflow-auto">
        {state.loading ? (
          <div className="flex justify-center py-10">
            <Spinner className="text-muted-foreground" />
          </div>
        ) : state.error !== null ? (
          <div className="flex flex-col items-center gap-3 px-6 py-10 text-center">
            <p className="text-sm text-muted-foreground">{t("preview.loadFailed")}</p>
            <p className="max-w-full break-words text-xs text-destructive-foreground">{state.error}</p>
            <Button size="xs" variant="outline" onClick={() => setAttempt((n) => n + 1)}>
              <RefreshCwIcon />
              {t("common.retry")}
            </Button>
          </div>
        ) : content?.kind === "text" ? (
          <>
            {isMarkdown && mode === "preview" ? (
              <div className="px-5 py-4 text-sm leading-relaxed">
                <Markdown text={content.text} />
              </div>
            ) : (
              <SourceView text={content.text} language={language} line={line} />
            )}
            {content.truncated ? (
              <p className="sticky bottom-0 border-t bg-background/95 px-4 py-1.5 text-xs text-muted-foreground">{t("preview.truncated")}</p>
            ) : null}
          </>
        ) : content?.kind === "image" ? (
          <div className="flex min-h-full items-center justify-center p-4">
            <img
              src={`data:${content.mimeType};base64,${content.base64}`}
              alt={path}
              className="max-h-[75vh] max-w-full rounded-lg border object-contain"
            />
          </div>
        ) : content?.kind === "unsupported" ? (
          <div className="flex min-h-full items-center justify-center p-6">
            <div className="max-w-sm rounded-xl border bg-muted/40 px-4 py-6 text-center text-sm text-muted-foreground">{content.reason}</div>
          </div>
        ) : null}
      </div>
    </>
  );
}
