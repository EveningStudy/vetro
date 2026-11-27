import { Dialog as DialogPrimitive } from "@base-ui/react/dialog";
import { CornerDownLeftIcon, MoonIcon, PaletteIcon, PanelLeftIcon, SearchIcon, SettingsIcon, SquarePenIcon, SunIcon } from "lucide-react";
import { type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { client } from "~/api/client";
import type { SearchHit, ThreadSummary } from "~/api/types";
import { ensureLocale, loadedTranslations, useT } from "~/i18n";
import { relativeTime } from "~/lib/format";
import { cn, isMac } from "~/lib/utils";
import { setTheme, setThemeId, usePrefs } from "~/state/prefs";
import { BUILT_IN_THEMES, type BuiltInTheme, type CustomTheme, resolveTheme } from "~/styles/themes";
import { actions, isCatalogStub, isExcludedCodexThread, useHiddenFilter, useStore } from "~/state/store";
import { AgentIcon } from "./AgentIcon";
import { ThemeSwatchDot, themeDisplayName, usePreviewMode } from "./settings/ThemeStudio";
import { Kbd, Spinner } from "./ui/primitives";

const THEME_WORDS = [
  "theme",
  "themes",
  "palette",
  "color",
  "colour",
  "appearance",
  "主题",
  "配色",
  "颜色",
  "外观",
  "皮肤",
  "主題",
  "顏色",
  "外觀",
  "テーマ",
  "外観",
  "테마",
  "외관",
  "tema",
  "apariencia",
  "thema",
  "design",
  "thème",
  "apparence",
];
const PAPER_WORDS = ["paper", "warm", "parchment", "sepia", "yellow", "纸", "暖", "黄", "护眼", "书", "紙", "和紙", "종이", "papel", "papier"];

function normalizeQuery(text: string) {
  return text.trim().toLowerCase();
}

/** Themes whose name (in English or any loaded locale), id or category matches the query. */
function matchThemes(query: string, themes: (BuiltInTheme | CustomTheme)[]) {
  const q = normalizeQuery(query);
  if (!q || (q.length < 2 && /^[\x20-\x7e]+$/.test(q))) return [];
  if (THEME_WORDS.some((word) => word.startsWith(q) || q.startsWith(word))) return themes;
  const paperQuery = PAPER_WORDS.some((word) => q.includes(word));
  return themes.filter((theme) => {
    const names = "nameKey" in theme ? loadedTranslations(theme.nameKey) : [theme.name];
    if (names.some((name) => name.toLowerCase().includes(q)) || theme.id.includes(q)) return true;
    return paperQuery && "category" in theme && theme.category === "paper";
  });
}

type Entry = { key: string; icon: ReactNode; title: ReactNode; detail?: ReactNode; context?: string; meta?: string; run: () => void };

const ENTITIES: Record<string, string> = { "&amp;": "&", "&lt;": "<", "&gt;": ">", "&quot;": '"', "&#39;": "'", "&#x27;": "'" };
const decode = (text: string) => text.replace(/&(?:amp|lt|gt|quot|#39|#x27);/g, (entity) => ENTITIES[entity] ?? entity);

/** The core escapes snippet text and wraps matches in <mark>; nothing else is markup. */
function Snippet({ html }: { html: string }) {
  const parts = html.split(/(<mark>[\s\S]*?<\/mark>)/g);
  return (
    <span className="search-snippet">
      {parts.map((part, index) =>
        part.startsWith("<mark>") ? <mark key={index}>{decode(part.slice(6, -7))}</mark> : <span key={index}>{decode(part)}</span>,
      )}
    </span>
  );
}

export function CommandPalette() {
  const t = useT();
  const open = useStore((s) => s.paletteOpen);
  const threads = useStore((s) => s.threads);
  const agents = useStore((s) => s.agents);
  const projects = useStore((s) => s.projects);
  const visible = useHiddenFilter();
  const showHidden = useStore((s) => s.filters.showHidden);
  const themeId = usePrefs((s) => s.themeId);
  const customThemes = usePrefs((s) => s.customThemes);
  const previewMode = usePreviewMode();
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
    if (/[\u4e00-\u9fff]/.test(q)) {
      void ensureLocale("zh");
      void ensureLocale("zh-Hant");
    }
    if (!q) {
      setHits(null);
      return;
    }
    setSearching(true);
    const handle = setTimeout(() => {
      client()
        .request("search.threads", { query: q, limit: 30, includeHidden: showHidden })
        .then((result) => setHits(result.hits.filter((hit) => !isExcludedCodexThread(hit.thread))))
        .catch(() => setHits([]))
        .finally(() => setSearching(false));
    }, 120);
    return () => clearTimeout(handle);
  }, [query, showHidden]);

  const openThread = (thread: ThreadSummary) => {
    close();
    actions.navigate({ view: "thread", threadId: thread.id });
  };

  const sections = useMemo(() => {
    const where = (thread: ThreadSummary) => {
      const agent = agents.find((a) => a.id === thread.agentId)?.name ?? thread.agentId;
      const cwd = (thread.cwd ?? "").toLowerCase();
      const project = projects.find((p) => p.path.toLowerCase() === cwd)?.name ?? thread.cwd?.split(/[\\/]/).filter(Boolean).at(-1);
      return project ? `${agent} · ${project}` : agent;
    };
    const threadEntry = (thread: ThreadSummary, detail?: ReactNode): Entry => ({
      key: thread.id,
      icon: <AgentIcon agentId={thread.agentId} />,
      title: thread.title,
      detail,
      context: where(thread),
      meta: relativeTime(thread.updatedAt, t),
      run: () => openThread(thread),
    });
    const activeTheme = resolveTheme(themeId, customThemes).theme.id;
    const themeEntry = (theme: BuiltInTheme | CustomTheme): Entry => ({
      key: `theme:${theme.id}`,
      icon: <ThemeSwatchDot theme={theme} mode={previewMode} />,
      title: themeDisplayName(theme, t),
      meta: theme.id === activeTheme ? t("palette.current") : undefined,
      run: () => {
        setThemeId(theme.id);
        close();
      },
    });
    const themeMatches = matchThemes(query, [...BUILT_IN_THEMES, ...customThemes]).map(themeEntry);
    if (hits !== null || themeMatches.length > 0) {
      return [
        { label: t("palette.themes"), entries: themeMatches },
        { label: t("palette.results"), entries: (hits ?? []).map((hit) => threadEntry(hit.thread, <Snippet html={hit.snippet} />)) },
      ];
    }
    const recent = Object.values(threads)
      .filter((thread) => !thread.archived && !isCatalogStub(thread) && visible(thread))
      .sort((a, b) => b.updatedAt - a.updatedAt)
      .slice(0, 8)
      .map((thread) => threadEntry(thread));
    const dark = document.documentElement.classList.contains("dark");
    const commands: Entry[] = [
      {
        key: "new",
        icon: <SquarePenIcon className="size-4 text-muted-foreground" />,
        title: t("sidebar.newThread"),
        meta: isMac ? "⌘N" : "Ctrl N",
        run: () => {
          close();
          actions.navigate({ view: "new", projectPath: null, agentId: null });
        },
      },
      {
        key: "sidebar",
        icon: <PanelLeftIcon className="size-4 text-muted-foreground" />,
        title: t("sidebar.toggle"),
        meta: isMac ? "⌘B" : "Ctrl B",
        run: () => {
          actions.toggleSidebar();
          close();
        },
      },
      {
        key: "settings",
        icon: <SettingsIcon className="size-4 text-muted-foreground" />,
        title: t("sidebar.settings"),
        run: () => {
          close();
          actions.navigate({ view: "settings", section: "general" });
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
      {
        key: "themes",
        icon: <PaletteIcon className="size-4 text-muted-foreground" />,
        title: t("palette.browseThemes"),
        run: () => {
          close();
          actions.navigate({ view: "settings", section: "appearance" });
        },
      },
    ];
    return [
      { label: t("palette.recent"), entries: recent },
      { label: t("palette.actions"), entries: commands },
    ];
  }, [hits, threads, t, visible, agents, projects, query, themeId, customThemes, previewMode]);

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
                              <span className="flex min-w-0 items-baseline gap-2">
                                <span className="truncate text-sm text-foreground">{entry.title}</span>
                                {entry.context ? <span className="shrink-0 text-xs text-muted-foreground">{entry.context}</span> : null}
                              </span>
                              {entry.detail ? <span className="line-clamp-2 text-xs text-muted-foreground">{entry.detail}</span> : null}
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
