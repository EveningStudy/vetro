import { Dialog as DialogPrimitive } from "@base-ui/react/dialog";
import { CornerDownLeftIcon, MoonIcon, SearchIcon, SettingsIcon, SquarePenIcon, SunIcon } from "lucide-react";
import { type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { client } from "~/api/client";
import type { SearchHit, ThreadSummary } from "~/api/types";
import { useT } from "~/i18n";
import { relativeTime } from "~/lib/format";
import { cn } from "~/lib/utils";
import { setTheme } from "~/state/prefs";
import { actions, useHiddenFilter, useStore } from "~/state/store";
import { AgentIcon } from "./AgentIcon";
import { Kbd, Spinner } from "./ui/primitives";

type Entry = { key: string; icon: ReactNode; title: ReactNode; detail?: ReactNode; meta?: string; run: () => void };

function Snippet({ html }: { html: string }) {
  const parts = html.split(/(<mark>.*?<\/mark>)/g);
  return (
    <span className="search-snippet">
      {parts.map((part, index) =>
        part.startsWith("<mark>") ? <mark key={index}>{part.slice(6, -7)}</mark> : <span key={index}>{part}</span>,
      )}
    </span>
  );
}

export function CommandPalette() {
  const t = useT();
  const open = useStore((s) => s.paletteOpen);
  const threads = useStore((s) => s.threads);
  const visible = useHiddenFilter();
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [active, setActive] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);

  const close = () => useStore.setState({ paletteOpen: false });

  useEffect(() => {
    if (!open) {
      setQuery("");
      setHits(null);
    }
  }, [open]);

  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setHits(null);
      return;
    }
    setSearching(true);
    const handle = setTimeout(() => {
      client()
        .request("search.threads", { query: q, limit: 30 })
        .then((result) => setHits(result.hits))
        .catch(() => setHits([]))
        .finally(() => setSearching(false));
    }, 120);
    return () => clearTimeout(handle);
  }, [query]);

  const openThread = (thread: ThreadSummary) => {
    close();
    actions.navigate({ view: "thread", threadId: thread.id });
  };

  const sections = useMemo(() => {
    const threadEntry = (thread: ThreadSummary, detail?: ReactNode): Entry => ({
      key: thread.id,
      icon: <AgentIcon agentId={thread.agentId} />,
      title: thread.title,
      detail,
      meta: relativeTime(thread.updatedAt, t),
      run: () => openThread(thread),
    });
    if (hits !== null) return [{ label: t("palette.results"), entries: hits.filter((hit) => visible(hit.thread)).map((hit) => threadEntry(hit.thread, <Snippet html={hit.snippet} />)) }];
    const recent = Object.values(threads)
      .filter((thread) => !thread.archived && visible(thread))
      .sort((a, b) => b.updatedAt - a.updatedAt)
      .slice(0, 8)
      .map((thread) => threadEntry(thread, thread.cwd ? <span className="font-mono text-[11px]">{thread.cwd}</span> : undefined));
    const dark = document.documentElement.classList.contains("dark");
    const commands: Entry[] = [
      {
        key: "new",
        icon: <SquarePenIcon className="size-4 text-muted-foreground" />,
        title: t("sidebar.newThread"),
        run: () => {
          close();
          actions.navigate({ view: "new", projectPath: null, agentId: null });
        },
      },
      {
        key: "settings",
        icon: <SettingsIcon className="size-4 text-muted-foreground" />,
        title: t("sidebar.settings"),
        run: () => {
          close();
          actions.navigate({ view: "settings", section: "agents" });
        },
      },
      {
        key: "theme",
        icon: dark ? <SunIcon className="size-4 text-muted-foreground" /> : <MoonIcon className="size-4 text-muted-foreground" />,
        title: `${t("settings.theme")}: ${dark ? t("settings.themeLight") : t("settings.themeDark")}`,
        run: () => {
          setTheme(dark ? "light" : "dark");
          close();
        },
      },
    ];
    return [
      { label: t("palette.recent"), entries: recent },
      { label: t("palette.actions"), entries: commands },
    ];
  }, [hits, threads, t, visible]);

  const flat = sections.flatMap((section) => section.entries);
  useEffect(() => setActive(0), [hits, query]);
  useEffect(() => {
    listRef.current?.querySelector(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [active]);

  let index = -1;
  return (
    <DialogPrimitive.Root open={open} onOpenChange={(next) => useStore.setState({ paletteOpen: next })}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Backdrop className="dialog-backdrop fixed inset-0 z-50 transition-all duration-150 data-ending-style:opacity-0 data-starting-style:opacity-0" />
        <div className="pointer-events-none fixed inset-0 z-50 flex items-start justify-center px-3 pt-[max(1rem,12vh)]">
          <DialogPrimitive.Popup className="dialog-glass pointer-events-auto flex max-h-[min(34rem,76vh)] w-full max-w-xl flex-col overflow-hidden rounded-2xl border outline-none transition-[scale,opacity] duration-150 data-ending-style:scale-98 data-ending-style:opacity-0 data-starting-style:scale-98 data-starting-style:opacity-0">
            <DialogPrimitive.Title className="sr-only">{t("sidebar.search")}</DialogPrimitive.Title>
            <div className="flex items-center gap-2.5 border-b px-4">
              {searching ? <Spinner className="size-4 text-muted-foreground" /> : <SearchIcon className="size-4 text-muted-foreground" />}
              <input
                autoFocus
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder={t("palette.placeholder")}
                onKeyDown={(event) => {
                  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                    event.preventDefault();
                    const delta = event.key === "ArrowDown" ? 1 : -1;
                    setActive((value) => (value + delta + flat.length) % Math.max(1, flat.length));
                  } else if (event.key === "Enter") {
                    event.preventDefault();
                    flat[active]?.run();
                  }
                }}
                className="h-12 min-w-0 flex-1 bg-transparent text-base outline-none placeholder:text-placeholder sm:text-sm"
              />
              <Kbd className="max-sm:hidden">Esc</Kbd>
            </div>
            <div ref={listRef} className="min-h-0 overflow-y-auto p-2">
              {flat.length === 0 ? (
                <div className="px-3 py-8 text-center text-sm text-muted-foreground">{t("palette.empty")}</div>
              ) : (
                sections.map((section) =>
                  section.entries.length === 0 ? null : (
                    <div key={section.label} className="mb-1">
                      <div className="px-2 pt-1.5 pb-1 text-xs font-medium text-muted-foreground">{section.label}</div>
                      {section.entries.map((entry) => {
                        index += 1;
                        const current = index;
                        return (
                          <button
                            key={entry.key}
                            type="button"
                            data-index={current}
                            onMouseMove={() => setActive(current)}
                            onClick={entry.run}
                            className={cn("flex w-full min-w-0 items-center gap-3 rounded-lg px-2 py-2 text-left", active === current && "bg-accent")}
                          >
                            <span className="flex size-5 shrink-0 items-center justify-center">{entry.icon}</span>
                            <span className="flex min-w-0 flex-1 flex-col">
                              <span className="truncate text-sm text-foreground">{entry.title}</span>
                              {entry.detail ? <span className="truncate text-xs text-muted-foreground">{entry.detail}</span> : null}
                            </span>
                            {entry.meta ? <span className="shrink-0 text-xs tabular-nums text-muted-foreground">{entry.meta}</span> : null}
                            {active === current ? <CornerDownLeftIcon className="size-3.5 shrink-0 text-muted-foreground" /> : null}
                          </button>
                        );
                      })}
                    </div>
                  ),
                )
              )}
            </div>
          </DialogPrimitive.Popup>
        </div>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}
