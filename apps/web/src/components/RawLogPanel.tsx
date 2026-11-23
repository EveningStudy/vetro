import { CopyIcon, RefreshCwIcon, XIcon } from "lucide-react";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { client } from "~/api/client";
import type { RawEntry } from "~/api/types";
import { useT } from "~/i18n";
import { clockTime } from "~/lib/format";
import { cn, copyText } from "~/lib/utils";
import { useStore } from "~/state/store";
import { errorMessage, toast } from "~/state/toast";
import { Button } from "./ui/button";
import { Tip } from "./ui/overlay";
import { Spinner, Switch } from "./ui/primitives";
import { linesForThread } from "./rawFilter";

type Direction = RawEntry["direction"];
const DIRECTIONS: Direction[] = ["spawn", "sent", "received", "stderr"];

const directionClass: Record<Direction, string> = {
  sent: "bg-info/10 text-info-foreground",
  received: "bg-success/10 text-success-foreground",
  stderr: "bg-warning/10 text-warning-foreground",
  spawn: "bg-foreground/8 text-foreground",
};

const RawLine = memo(function RawLine({ entry }: { entry: RawEntry }) {
  const t = useT();
  return (
    <div className="group/raw flex gap-2 border-b border-border/60 px-3 py-1.5 hover:bg-accent/50">
      <div className="flex w-[5.75rem] shrink-0 flex-col items-start gap-1">
        <span className="tabular-nums text-muted-foreground">{clockTime(entry.ts, true)}</span>
        <span className={cn("rounded px-1 py-px text-[10px] font-medium uppercase tracking-wide", directionClass[entry.direction])}>
          {t(`raw.dir.${entry.direction}`)}
        </span>
      </div>
      <pre className="min-w-0 flex-1 whitespace-pre-wrap break-all text-foreground/90 select-text">{entry.text}</pre>
      <button
        type="button"
        aria-label={t("common.copy")}
        onClick={() => void copyText(entry.text).then(() => toast.success(t("common.copied")))}
        className="flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground opacity-0 hover:bg-accent hover:text-foreground group-hover/raw:opacity-100 pointer-coarse:opacity-100"
      >
        <CopyIcon className="size-3" />
      </button>
    </div>
  );
});

/** The wire log: the evidence that Vetro forwards exactly what the user typed and nothing else. */
export function RawLogPanel({ threadId }: { threadId: string }) {
  const t = useT();
  const [entries, setEntries] = useState<RawEntry[] | null>(null);
  const [filter, setFilter] = useState<Direction | null>(null);
  const [onlyThread, setOnlyThread] = useState(true);
  const nativeId = useStore((s) => s.threads[threadId]?.nativeId ?? s.open[threadId]?.summary.nativeId ?? threadId.slice(threadId.indexOf(":") + 1));
  const [loading, setLoading] = useState(false);
  const version = useStore((s) => `${s.open[threadId]?.items.length ?? 0}:${s.open[threadId]?.summary.status ?? ""}`);
  const listRef = useRef<HTMLDivElement>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const result = await client().request("threads.raw", { threadId, limit: 2000 });
      setEntries(result.entries);
    } catch (error) {
      toast.error(errorMessage(error));
    } finally {
      setLoading(false);
    }
  }, [threadId]);

  useEffect(() => {
    setEntries(null);
  }, [threadId]);

  useEffect(() => {
    const handle = setTimeout(() => void load(), 150);
    return () => clearTimeout(handle);
  }, [load, version]);

  useEffect(() => {
    const list = listRef.current;
    if (list) list.scrollTop = list.scrollHeight;
  }, [entries]);

  const scoped = useMemo(() => {
    if (!entries || !onlyThread) return entries ?? [];
    const keep = linesForThread(entries, nativeId);
    return entries.filter((_, index) => keep.has(index));
  }, [entries, onlyThread, nativeId]);
  const shown = scoped.filter((entry) => !filter || entry.direction === filter);
  const otherLines = (entries?.length ?? 0) - scoped.length;
  const close = () => useStore.setState({ rawPanelOpen: false });

  return (
    <aside className="fixed inset-0 z-40 flex min-h-0 flex-col bg-background pt-[env(safe-area-inset-top)] md:static md:z-auto md:w-[min(30rem,45vw)] md:shrink-0 md:border-l md:pt-0 lg:w-[32rem]">
      <div className="flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 ps-4 pe-2">
        <div className="min-w-0 flex-1">
          <div className="text-sm font-medium">{t("raw.title")}</div>
          <div className="truncate text-xs text-muted-foreground">{t("raw.subtitle")}</div>
        </div>
        <Tip label={t("raw.refresh")}>
          <Button size="icon-sm" variant="ghost" aria-label={t("raw.refresh")} onClick={() => void load()}>
            {loading ? <Spinner className="size-3.5" /> : <RefreshCwIcon />}
          </Button>
        </Tip>
        <Button
          size="xs"
          variant="outline"
          disabled={!entries?.length}
          onClick={() =>
            void copyText(shown.map((entry) => JSON.stringify(entry)).join("\n")).then(() => toast.success(t("common.copied")))
          }
        >
          <CopyIcon />
          {t("raw.copyAll")}
        </Button>
        <Button size="icon-sm" variant="ghost" aria-label={t("common.close")} onClick={close}>
          <XIcon />
        </Button>
      </div>
      <div className="flex shrink-0 items-center gap-1 border-b px-3 pb-2">
        {[null, ...DIRECTIONS].map((direction) => (
          <button
            key={direction ?? "all"}
            type="button"
            aria-pressed={filter === direction}
            onClick={() => setFilter(direction)}
            className={cn(
              "h-6 rounded-md px-2 text-xs font-medium text-muted-foreground hover:bg-accent hover:text-foreground",
              filter === direction && "bg-accent text-foreground",
            )}
          >
            {direction ? t(`raw.dir.${direction}`) : t("raw.filterAll")}
          </button>
        ))}
        <span className="ms-auto text-xs tabular-nums text-muted-foreground">{t("raw.entries", { n: shown.length })}</span>
      </div>
      <label className="flex shrink-0 cursor-pointer items-center gap-2 border-b px-4 py-2 text-xs text-muted-foreground">
        <Switch checked={onlyThread} onCheckedChange={setOnlyThread} />
        <span className="min-w-0 flex-1">{t("raw.onlyThread")}</span>
        {onlyThread && otherLines > 0 ? <span className="shrink-0 tabular-nums">{t("raw.otherLines", { n: otherLines })}</span> : null}
      </label>
      <div ref={listRef} className="min-h-0 flex-1 overflow-y-auto font-mono text-[11.5px] leading-relaxed">
        {entries === null ? (
          <div className="flex justify-center py-10">
            <Spinner className="text-muted-foreground" />
          </div>
        ) : entries.length === 0 ? (
          <p className="px-4 py-10 text-center font-sans text-sm text-muted-foreground">{t("raw.empty")}</p>
        ) : (
          shown.map((entry, index) => <RawLine key={`${entry.ts}-${index}`} entry={entry} />)
        )}
      </div>
    </aside>
  );
}
