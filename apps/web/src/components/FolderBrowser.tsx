import { ArrowUpIcon, CheckIcon, ChevronRightIcon, FileIcon, FolderIcon, FolderOpenIcon, FolderPlusIcon, HardDriveIcon, XIcon } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { client } from "~/api/client";
import type { FsEntry } from "~/api/types";
import { useT } from "~/i18n";
import { pickSystemFolder, useDesktopChrome } from "~/lib/desktopChrome";
import { cn } from "~/lib/utils";
import { useIsLocalDesktop } from "~/state/store";
import { errorMessage, toast } from "~/state/toast";
import { Button } from "./ui/button";
import { Dialog, DialogPopup, DialogTitle, Tip } from "./ui/overlay";
import { Input, Spinner } from "./ui/primitives";

export function FolderBrowser({ open, onOpenChange, onChoose }: { open: boolean; onOpenChange: (open: boolean) => void; onChoose: (path: string) => void }) {
  const t = useT();
  const local = useIsLocalDesktop();
  const { chrome } = useDesktopChrome();
  const [path, setPath] = useState<string | undefined>(undefined);
  const [listing, setListing] = useState<{ path: string; parent: string | null; entries: FsEntry[] } | null>(null);
  const [loading, setLoading] = useState(false);
  const [typed, setTyped] = useState("");
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");
  const [making, setMaking] = useState(false);
  const newInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!open) {
      setCreating(false);
      setNewName("");
      return;
    }
    setLoading(true);
    client()
      .request("fs.list", path === undefined ? {} : { path })
      .then((result) => {
        setListing(result);
        setTyped(result.path);
      })
      .catch((error) => toast.error(errorMessage(error)))
      .finally(() => setLoading(false));
  }, [open, path]);

  useEffect(() => {
    if (creating) requestAnimationFrame(() => newInputRef.current?.focus());
  }, [creating]);

  const atRoots = listing?.path === "";
  const canSystemPick = local && chrome !== "web";

  const createFolder = async () => {
    if (!listing || atRoots || making) return;
    const name = newName.trim();
    if (!name) return;
    setMaking(true);
    try {
      const result = await client().request("fs.mkdir", { parent: listing.path, name });
      setCreating(false);
      setNewName("");
      setPath(result.path);
    } catch (error) {
      toast.error(errorMessage(error));
    } finally {
      setMaking(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogPopup className="max-w-xl" position="top">
        <div className="flex items-center px-5 pt-5 pe-12">
          <DialogTitle>{t("folder.title")}</DialogTitle>
        </div>
        <form
          className="flex items-center gap-2 px-5 pt-4"
          onSubmit={(event) => {
            event.preventDefault();
            setPath(typed.trim());
          }}
        >
          <Button
            size="icon-sm"
            variant="outline"
            aria-label={t("folder.up")}
            disabled={listing?.parent === null || listing?.parent === undefined}
            onClick={() => listing?.parent !== null && listing?.parent !== undefined && setPath(listing.parent)}
          >
            <ArrowUpIcon />
          </Button>
          <Input
            aria-label={t("folder.path")}
            value={atRoots ? "" : typed}
            placeholder={t("folder.drives")}
            onChange={(event) => setTyped(event.target.value)}
            className="font-mono text-xs sm:text-xs"
          />
          {local ? (
            <Tip label={t("folder.newFolder")}>
              <Button
                size="icon-sm"
                variant="outline"
                type="button"
                aria-label={t("folder.newFolder")}
                disabled={!listing || atRoots}
                onClick={() => {
                  setCreating(true);
                  setNewName("");
                }}
              >
                <FolderPlusIcon />
              </Button>
            </Tip>
          ) : null}
        </form>
        <div className="mx-5 mt-3 h-80 overflow-y-auto rounded-lg border bg-background/50 p-1">
          {creating ? (
            <form
              className="mb-1 flex items-center gap-1.5 rounded-md bg-accent/60 px-2 py-1"
              onSubmit={(event) => {
                event.preventDefault();
                void createFolder();
              }}
            >
              <FolderPlusIcon className="size-4 shrink-0 text-muted-foreground" />
              <input
                ref={newInputRef}
                value={newName}
                disabled={making}
                placeholder={t("folder.newFolderPlaceholder")}
                aria-label={t("folder.newFolder")}
                onChange={(event) => setNewName(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Escape") {
                    event.preventDefault();
                    event.stopPropagation();
                    setCreating(false);
                    setNewName("");
                  }
                }}
                className="h-7 min-w-0 flex-1 bg-transparent text-sm outline-none placeholder:text-placeholder"
              />
              <Button size="icon-xs" variant="ghost" type="submit" disabled={!newName.trim() || making} aria-label={t("folder.create")}>
                {making ? <Spinner className="size-3.5" /> : <CheckIcon />}
              </Button>
              <Button
                size="icon-xs"
                variant="ghost"
                type="button"
                aria-label={t("common.cancel")}
                onClick={() => {
                  setCreating(false);
                  setNewName("");
                }}
              >
                <XIcon />
              </Button>
            </form>
          ) : null}
          {loading && !listing ? (
            <div className="flex h-full items-center justify-center">
              <Spinner className="text-muted-foreground" />
            </div>
          ) : listing && listing.entries.length === 0 && !creating ? (
            <div className="flex h-full items-center justify-center text-sm text-muted-foreground">{t("folder.empty")}</div>
          ) : (
            listing?.entries.map((entry) => (
              <button
                key={entry.path}
                type="button"
                disabled={!entry.isDir}
                onClick={() => {
                  setCreating(false);
                  setPath(entry.path);
                }}
                className={cn(
                  "flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-sm",
                  entry.isDir ? "hover:bg-accent" : "cursor-default text-muted-foreground/70",
                )}
              >
                {atRoots ? (
                  <HardDriveIcon className="size-4 shrink-0 text-muted-foreground" />
                ) : entry.isDir ? (
                  <FolderIcon className="size-4 shrink-0 text-muted-foreground" />
                ) : (
                  <FileIcon className="size-4 shrink-0 text-muted-foreground/60" />
                )}
                <span className="min-w-0 flex-1 truncate">{entry.name}</span>
                {entry.isDir ? <ChevronRightIcon className="size-3.5 shrink-0 text-muted-foreground/70" /> : null}
              </button>
            ))
          )}
        </div>
        <div className="mt-4 flex items-center justify-between gap-2 rounded-b-2xl border-t bg-muted/60 px-5 py-3">
          <div>
            {canSystemPick ? (
              <Button
                variant="ghost"
                size="sm"
                type="button"
                onClick={async () => {
                  const chosen = await pickSystemFolder(listing && !atRoots ? listing.path : undefined);
                  if (chosen) {
                    onChoose(chosen);
                    onOpenChange(false);
                  }
                }}
              >
                <FolderOpenIcon />
                {t("folder.systemPicker")}
              </Button>
            ) : null}
          </div>
          <div className="flex items-center gap-2">
            <Button variant="ghost" onClick={() => onOpenChange(false)}>
              {t("common.cancel")}
            </Button>
            <Button
              disabled={!listing || atRoots}
              onClick={() => {
                if (!listing) return;
                onChoose(listing.path);
                onOpenChange(false);
              }}
            >
              {t("folder.choose")}
            </Button>
          </div>
        </div>
      </DialogPopup>
    </Dialog>
  );
}
