import { CopyIcon, SquareArrowOutUpRightIcon } from "lucide-react";
import { useT } from "~/i18n";
import { cn, copyText } from "~/lib/utils";
import { actions, useIsLocalDesktop } from "~/state/store";
import { toast } from "~/state/toast";
import { Tip } from "./ui/overlay";

/** Opens a file in the user's editor on the desktop; a paired device can only copy the path. */
export function OpenFileButton({ cwd, path, line, className }: { cwd: string | null; path: string; line?: number | null; className?: string }) {
  const t = useT();
  const local = useIsLocalDesktop();
  const canOpen = local && !!cwd;
  const label = canOpen ? t("open.editor") : t("open.copyPath");
  return (
    <Tip label={label}>
      <button
        type="button"
        aria-label={label}
        onClick={(event) => {
          event.stopPropagation();
          if (canOpen) void actions.openExternal(cwd, "editor", path, line ?? undefined);
          else void copyText(path).then((ok) => ok && toast.success(t("common.copied")));
        }}
        className={cn(
          "inline-flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
          className,
        )}
      >
        {canOpen ? <SquareArrowOutUpRightIcon className="size-3" /> : <CopyIcon className="size-3" />}
      </button>
    </Tip>
  );
}
