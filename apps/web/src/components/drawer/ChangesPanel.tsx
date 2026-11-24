import { ChevronRightIcon, FileDiffIcon, GitBranchIcon, RefreshCwIcon, XIcon } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { client } from "~/api/client";
import type { GitDiffResult, GitFileChange, Item, ToolContent } from "~/api/types";
import { type MessageKey, useT } from "~/i18n";
import { cn } from "~/lib/utils";
import { actions, useStore } from "~/state/store";
import { errorMessage } from "~/state/toast";
import { ThreadCwdContext } from "../chat/threadContext";
import { DiffBlock, diffStats } from "../chat/ToolDetails";
import { OpenFileButton } from "../OpenFileButton";
import { Button } from "../ui/button";
import { Tip } from "../ui/overlay";
import { Spinner } from "../ui/primitives";

type DiffContent = Extract<ToolContent, { type: "diff" }>;

export interface EditedFile {
  path: string;
  additions: number;
  deletions: number;
  blocks: DiffContent[];
}

const EDIT_KINDS = new Set(["edit", "delete", "move"]);

type BlockStats = { block: DiffContent; additions: number; deletions: number };
/** Items keep their identity until they change, so diff stats are computed once per item. */
const statsCache = new WeakMap<Item, BlockStats[]>();

function blockStats(item: Extract<Item, { type: "tool_call" }>) {
  let cached = statsCache.get(item);
  if (!cached) {
    cached = item.content.flatMap((block) => (block.type === "diff" ? [{ block, ...diffStats([block]) }] : []));
    statsCache.set(item, cached);
  }
  return cached;
}

/** Files this thread's own tool calls changed, for folders that are not Git repositories. */
export function editedFiles(items: Item[]): EditedFile[] {
  const files = new Map<string, EditedFile>();
  for (const item of items) {
    if (item.type !== "tool_call" || !EDIT_KINDS.has(item.kind) || item.status === "failed") continue;
    for (const { block, additions, deletions } of blockStats(item)) {
      const entry = files.get(block.path) ?? { path: block.path, additions: 0, deletions: 0, blocks: [] };
      entry.additions += additions;
      entry.deletions += deletions;
      entry.blocks.push(block);
      files.set(block.path, entry);
    }
  }
  return [...files.values()];
}

export function useEditedFiles(threadId: string) {
  const items = useStore((s) => s.open[threadId]?.items);
  return useMemo(() => editedFiles(items ?? []), [items]);
}

const STATUS_LETTER: Record<string, string> = { modified: "M", added: "A", deleted: "D", renamed: "R", untracked: "U" };
const STATUS_CLASS: Record<string, string> = {
  modified: "text-warning-foreground",
  added: "text-success-foreground",
  untracked: "text-success-foreground",
  deleted: "text-destructive-foreground",
  renamed: "text-info-foreground",
};

function splitPath(path: string) {
  const index = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return index < 0 ? { dir: "", name: path } : { dir: path.slice(0, index + 1), name: path.slice(index + 1) };
}

function Counts({ additions, deletions }: { additions: number | null; deletions: number | null }) {
  if (!additions && !deletions) return null;
  return (
    <span className="shrink-0 font-mono text-xs tabular-nums">
      {additions ? <span className="text-diff-addition-foreground">+{additions}</span> : null}
      {additions && deletions ? " " : null}
      {deletions ? <span className="text-diff-deletion-foreground">−{deletions}</span> : null}
    </span>
  );
}

function FileRow({
  path,
  status,
  additions,
  deletions,
  open,
  onToggle,
  openCwd,
}: {
  path: string;
  status?: string;
  additions: number | null;
  deletions: number | null;
  open: boolean;
  onToggle: () => void;
  openCwd: string | null;
}) {
  const t = useT();
  const { dir, name } = splitPath(path);
  return (
    <div
      role="button"
      tabIndex={0}
      aria-expanded={open}
      onClick={onToggle}
      onKeyDown={(event) => {
        if (event.target === event.currentTarget && (event.key === "Enter" || event.key === " ")) {
          event.preventDefault();
          onToggle();
        }
      }}
      title={path}
      className="group/file flex h-8 cursor-pointer select-none items-center gap-2 rounded-md px-2 text-sm outline-none hover:bg-accent/60 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
    >
      <ChevronRightIcon className={cn("size-3.5 shrink-0 text-muted-foreground transition-transform", open && "rotate-90")} />
      {status ? (
        <span
          className={cn("w-3 shrink-0 text-center font-mono text-[11px] font-semibold", STATUS_CLASS[status] ?? "text-muted-foreground")}
          title={t(`changes.status.${STATUS_LETTER[status] ? status : "other"}` as MessageKey)}
        >
          {STATUS_LETTER[status] ?? "?"}
        </span>
      ) : null}
      <span className="flex min-w-0 flex-1 items-baseline font-mono text-xs">
        <span className="truncate text-muted-foreground">{dir}</span>
        <span className="shrink-0 text-foreground">{name}</span>
      </span>
      <Counts additions={additions} deletions={deletions} />
      <OpenFileButton cwd={openCwd} path={path} className="opacity-0 group-hover/file:opacity-100 focus-visible:opacity-100 pointer-coarse:opacity-100" />
    </div>
  );
}

type DiffState = { loading: true } | { loading: false; result: GitDiffResult | null; error: string | null };

function GitDiffView({ state, path }: { state: DiffState | undefined; path: string }) {
  const t = useT();
  if (!state || state.loading)
    return (
      <div className="flex justify-center py-3">
        <Spinner className="size-3.5 text-muted-foreground" />
      </div>
    );
  if (state.error) return <p className="px-3 py-2 text-xs text-destructive-foreground">{state.error}</p>;
  const result = state.result;
  if (!result) return null;
  if (result.binary) return <p className="px-3 py-2 text-xs text-muted-foreground">{t("changes.binary")}</p>;
  if (!result.diff && result.newText === null && result.oldText === null)
    return <p className="px-3 py-2 text-xs text-muted-foreground">{t("changes.noText")}</p>;
  const block: DiffContent = { type: "diff", path, oldText: result.oldText, newText: result.newText, unified: result.diff };
  return (
    <>
      <DiffBlock block={block} bare />
      {result.truncated ? <p className="border-t px-3 py-1.5 text-xs text-muted-foreground">{t("changes.truncated")}</p> : null}
    </>
  );
}

function Expanded({ children }: { children: React.ReactNode }) {
  return <div className="mx-2 mt-0.5 mb-2 overflow-hidden rounded-lg border bg-muted/60 dark:bg-white/[0.02]">{children}</div>;
}

/** Read-only view of what changed in the thread's folder: Git's working tree, or this thread's own
 * edits when the folder is not a repository. */
export function ChangesPanel({ threadId }: { threadId: string }) {
  const t = useT();
  const cwd = useStore((s) => (s.threads[threadId] ?? s.open[threadId]?.summary)?.cwd ?? null);
  const status = useStore((s) => (cwd ? s.gitStatus[cwd] : undefined));
  const edited = useEditedFiles(threadId);
  const [refreshing, setRefreshing] = useState(false);
  const [open, setOpen] = useState<Record<string, boolean>>({});
  const [diffs, setDiffs] = useState<Record<string, DiffState>>({});

  const refresh = () => {
    if (!cwd) return;
    setRefreshing(true);
    setDiffs({});
    void actions.refreshGitStatus(cwd).finally(() => setRefreshing(false));
  };

  useEffect(() => {
    refresh();
  }, [cwd]);

  const loadDiff = (file: GitFileChange) => {
    if (!cwd) return;
    setDiffs((current) => ({ ...current, [file.path]: { loading: true } }));
    client()
      .request("workspace.gitDiff", { cwd, path: file.path })
      .then((result) => setDiffs((current) => ({ ...current, [file.path]: { loading: false, result, error: null } })))
      .catch((error: unknown) => setDiffs((current) => ({ ...current, [file.path]: { loading: false, result: null, error: errorMessage(error) } })));
  };

  const toggleGit = (file: GitFileChange) => {
    const next = !open[`g:${file.path}`];
    setOpen((current) => ({ ...current, [`g:${file.path}`]: next }));
    if (next && !diffs[file.path]) loadDiff(file);
  };

  const repo = status?.isRepo ? status : null;
  const files = repo?.files ?? [];
  const showEdited = edited.length > 0 && files.length === 0;
  const openCwd = repo?.root ?? cwd;

  return (
    <>
      <div className="flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 border-b ps-4 pe-2">
        <FileDiffIcon className="size-4 shrink-0 text-muted-foreground" />
        <span className="text-sm font-medium">{t("changes.title")}</span>
        {repo?.branch ? (
          <span className="inline-flex min-w-0 items-center gap-1 text-xs text-muted-foreground" title={repo.root ?? undefined}>
            <GitBranchIcon className="size-3 shrink-0" />
            <span className="truncate font-mono">{repo.branch}</span>
          </span>
        ) : null}
        <span className="flex-1" />
        <Tip label={t("changes.refresh")}>
          <Button size="icon-sm" variant="ghost" aria-label={t("changes.refresh")} disabled={!cwd || refreshing} onClick={refresh}>
            {refreshing ? <Spinner className="size-3.5" /> : <RefreshCwIcon />}
          </Button>
        </Tip>
        <Tip label={t("common.close")}>
          <Button size="icon-sm" variant="ghost" aria-label={t("common.close")} onClick={actions.closeDrawer}>
            <XIcon />
          </Button>
        </Tip>
      </div>
      <ThreadCwdContext value={openCwd}>
        <div className="min-h-0 flex-1 overflow-y-auto p-2">
          {!cwd ? (
            <p className="px-2 py-6 text-center text-sm text-muted-foreground">{t("changes.noFolder")}</p>
          ) : status === undefined && refreshing ? (
            <div className="flex justify-center py-10">
              <Spinner className="text-muted-foreground" />
            </div>
          ) : (
            <>
              {files.length > 0 ? (
                <div className="mb-1 px-2 pt-1 pb-1.5 text-xs text-muted-foreground">{t("changes.workingTree", { n: files.length })}</div>
              ) : null}
              {files.map((file) => (
                <div key={file.path}>
                  <FileRow
                    path={file.path}
                    status={file.status}
                    additions={file.additions}
                    deletions={file.deletions}
                    open={!!open[`g:${file.path}`]}
                    onToggle={() => toggleGit(file)}
                    openCwd={openCwd}
                  />
                  {open[`g:${file.path}`] ? (
                    <Expanded>
                      <GitDiffView state={diffs[file.path]} path={file.path} />
                    </Expanded>
                  ) : null}
                </div>
              ))}
              {showEdited ? (
                <>
                  <div className="mb-1 px-2 pt-1 pb-1.5 text-xs text-muted-foreground">
                    {repo ? t("changes.cleanEdited") : t("changes.notRepo")}
                  </div>
                  {edited.map((file) => (
                    <div key={file.path}>
                      <FileRow
                        path={file.path}
                        additions={file.additions}
                        deletions={file.deletions}
                        open={!!open[`e:${file.path}`]}
                        onToggle={() => setOpen((current) => ({ ...current, [`e:${file.path}`]: !current[`e:${file.path}`] }))}
                        openCwd={cwd}
                      />
                      {open[`e:${file.path}`] ? (
                        <Expanded>
                          {file.blocks.map((block, index) => (
                            <div key={index} className={cn(index > 0 && "border-t")}>
                              <DiffBlock block={block} bare />
                            </div>
                          ))}
                        </Expanded>
                      ) : null}
                    </div>
                  ))}
                </>
              ) : null}
              {files.length === 0 && !showEdited ? (
                <p className="px-2 py-6 text-center text-sm text-muted-foreground">{repo ? t("changes.clean") : t("changes.notRepoEmpty")}</p>
              ) : null}
            </>
          )}
        </div>
      </ThreadCwdContext>
    </>
  );
}
