import { CheckIcon, ChevronDownIcon, FolderIcon, FolderPlusIcon, FolderXIcon, SearchIcon } from "lucide-react";
import { useRef, useState } from "react";
import type { Project } from "~/api/types";
import { useT } from "~/i18n";
import { tildePath } from "~/lib/format";
import { cn } from "~/lib/utils";
import { Popover, PopoverPopup, PopoverTrigger } from "../ui/overlay";
import { composerControlClass } from "./OptionPicker";

/** Project folder for a new thread: searchable, with "Add folder…" always in reach. */
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
  onChange: (path: string) => void;
  onAddFolder: () => void;
}) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  const current = projects.find((project) => project.path === value);
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
  const filtered = projects.filter((project) => terms.every((term) => `${project.name} ${project.path}`.toLowerCase().includes(term)));
  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) setQuery("");
      }}
    >
      <PopoverTrigger className={cn(composerControlClass, "max-w-full font-normal text-muted-foreground")} aria-label={t("composer.project")}>
        <FolderIcon />
        <span className="min-w-0 truncate">{current?.name ?? (value ? tildePath(value, home) : t("composer.chooseProject"))}</span>
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
                if (event.key === "Enter" && filtered[0]) {
                  event.preventDefault();
                  onChange(filtered[0].path);
                  setOpen(false);
                }
              }}
              placeholder={`${t("composer.project")}…`}
              className="h-9 min-w-0 flex-1 bg-transparent text-sm outline-none placeholder:text-placeholder"
            />
          </div>
        ) : null}
        <div className="max-h-[min(18rem,calc(var(--available-height)-6rem))] overflow-y-auto p-1">
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
            onClick={() => {
              setOpen(false);
              onAddFolder();
            }}
            className="flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm hover:bg-accent"
          >
            <FolderPlusIcon className="size-4 shrink-0 text-muted-foreground" />
            {t("composer.addFolder")}
          </button>
        </div>
      </PopoverPopup>
    </Popover>
  );
}
