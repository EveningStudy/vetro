import { ArrowUpIcon, ChevronRightIcon, FileIcon, FolderIcon, HardDriveIcon } from "lucide-react";
import { useEffect, useState } from "react";
import { client } from "~/api/client";
import type { FsEntry } from "~/api/types";
import { useT } from "~/i18n";
import { cn } from "~/lib/utils";
import { errorMessage, toast } from "~/state/toast";
import { Button } from "./ui/button";
import { Dialog, DialogDescription, DialogPopup, DialogTitle } from "./ui/overlay";
import { Input, Spinner } from "./ui/primitives";

export function FolderBrowser({ open, onOpenChange, onChoose }: { open: boolean; onOpenChange: (open: boolean) => void; onChoose: (path: string) => void }) {
  const t = useT();
  const [path, setPath] = useState<string | undefined>(undefined);
  const [listing, setListing] = useState<{ path: string; parent: string | null; entries: FsEntry[] } | null>(null);
  const [loading, setLoading] = useState(false);
  const [typed, setTyped] = useState("");

  useEffect(() => {
    if (!open) return;
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

  const atRoots = listing?.path === "";
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogPopup className="max-w-xl" position="top">
        <div className="flex flex-col gap-1 px-5 pt-5 pe-12">
          <DialogTitle>{t("folder.title")}</DialogTitle>
          <DialogDescription>{t("folder.description")}</DialogDescription>
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
        </form>
        <div className="mx-5 mt-3 h-80 overflow-y-auto rounded-lg border bg-background/50 p-1">
          {loading && !listing ? (
            <div className="flex h-full items-center justify-center">
              <Spinner className="text-muted-foreground" />
            </div>
          ) : listing && listing.entries.length === 0 ? (
            <div className="flex h-full items-center justify-center text-sm text-muted-foreground">{t("folder.empty")}</div>
          ) : (
            listing?.entries.map((entry) => (
              <button
                key={entry.path}
                type="button"
                disabled={!entry.isDir}
                onClick={() => setPath(entry.path)}
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
        <div className="mt-4 flex items-center justify-end gap-2 border-t bg-muted/60 px-5 py-3 rounded-b-2xl">
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
      </DialogPopup>
    </Dialog>
  );
}
