import { CheckIcon, ChevronDownIcon, FolderIcon, FolderPlusIcon, FolderXIcon, MessageSquareIcon, SearchIcon } from "lucide-react";
import { useRef, useState } from "react";
import type { Project } from "~/api/types";
import { useT } from "~/i18n";
import { tildePath } from "~/lib/format";
import { cn } from "~/lib/utils";
import { useStore } from "~/state/store";
import { Popover, PopoverPopup, PopoverTrigger } from "../ui/overlay";
import { composerControlClass } from "./OptionPicker";

/** Project folder for a new thread: "Quick chat (no project)" or a searchable workspace folder. */
export function ProjectPicker({
  projects,
  value,
  home,
  onChange,
  onAddFolder,
}: {
  projects: Project[];
  value: string | null;
  home: string | null | undefined;
  onChange: (path: string | null) => void;
  onAddFolder: () => void;
}) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const remoteClient = useStore((s) => s.app?.remoteClient ?? false);
  const [query, setQuery] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  const current = projects.find((project) => project.path === value);
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
  const filtered = projects.filter((project) => terms.every((term) => `${project.name} ${project.path}`.toLowerCase().includes(term)));
  const quickMatch = terms.length === 0 || terms.every((term) => `${t("composer.quickChat")} ${t("composer.quickChatHint")} quick scratch`.toLowerCase().includes(term));
  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) setQuery("");
      }}
    >
      <PopoverTrigger className={cn(composerControlClass, "max-w-full font-normal text-muted-foreground")} aria-label={t("composer.project")}>
        {value ? <FolderIcon /> : <MessageSquareIcon />}
        <span className="min-w-0 truncate">{value ? (current?.name ?? tildePath(value, home)) : t("composer.quickChat")}</span>
        {value ? <span className="hidden min-w-0 truncate font-mono text-[11px] text-muted-foreground/70 sm:inline">{tildePath(value, home)}</span> : null}
        <ChevronDownIcon className="size-3 opacity-70" />
      </PopoverTrigger>
      <PopoverPopup side="bottom" align="start" className="w-[min(24rem,calc(100vw-2rem))]" initialFocus={inputRef}>
        {projects.length > 6 ? (
          <div className="flex items-center gap-2 border-b px-3">
            <SearchIcon className="size-3.5 shrink-0 text-muted-foreground" />
            <input
              ref={inputRef}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  if (filtered[0]) {
                    onChange(filtered[0].path);
                    setOpen(false);
                  } else if (quickMatch) {
                    onChange(null);
                    setOpen(false);
                  }
                }
              }}
              placeholder={`${t("composer.project")}…`}
              className="h-9 min-w-0 flex-1 bg-transparent text-sm outline-none placeholder:text-placeholder"
            />
          </div>
        ) : null}
        <div className="max-h-[min(18rem,calc(var(--available-height)-6rem))] overflow-y-auto p-1">
          {quickMatch ? (
            <>
              <button
                type="button"
                onClick={() => {
                  onChange(null);
                  setOpen(false);
                }}
                className="flex w-full min-w-0 items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm hover:bg-accent"
              >
                <MessageSquareIcon className="size-4 shrink-0 text-muted-foreground" />
                <span className="flex min-w-0 flex-1 flex-col">
                  <span className="truncate">{t("composer.quickChat")}</span>
                  <span className="truncate text-[11px] text-muted-foreground">{t("composer.quickChatHint")}</span>
                </span>
                {value === null ? <CheckIcon className="size-3.5 shrink-0" /> : null}
              </button>
              {filtered.length > 0 ? <div className="my-1 border-t" /> : null}
            </>
          ) : null}
          {filtered.map((project) => (
            <button
              key={project.path}
              type="button"
              disabled={!project.exists}
              onClick={() => {
                onChange(project.path);
                setOpen(false);
              }}
              className="flex w-full min-w-0 items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm hover:bg-accent disabled:opacity-50 disabled:hover:bg-transparent"
            >
              {project.exists ? <FolderIcon className="size-4 shrink-0 text-muted-foreground" /> : <FolderXIcon className="size-4 shrink-0 text-muted-foreground" />}
              <span className="flex min-w-0 flex-1 flex-col">
                <span className="truncate">{project.name}</span>
                <span className="truncate font-mono text-[11px] text-muted-foreground">{tildePath(project.path, home)}</span>
              </span>
              {project.path === value ? <CheckIcon className="size-3.5 shrink-0" /> : null}
            </button>
          ))}
        </div>
        <div className="border-t p-1">
          <button
            type="button"
            disabled={remoteClient}
            onClick={() => {
              setOpen(false);
              onAddFolder();
            }}
            className="flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm hover:bg-accent disabled:cursor-default disabled:opacity-60 disabled:hover:bg-transparent"
          >
            <FolderPlusIcon className="size-4 shrink-0 text-muted-foreground" />
            <span className="flex min-w-0 flex-col">
              <span>{t("composer.addFolder")}</span>
              {remoteClient ? <span className="text-xs text-muted-foreground">{t("remote.addFolderLocal")}</span> : null}
            </span>
          </button>
        </div>
      </PopoverPopup>
    </Popover>
  );
}
