import { SquarePenIcon } from "lucide-react";
import { useT } from "~/i18n";
import { actions } from "~/state/store";
import { cn, isMac } from "~/lib/utils";
import { MobileSidebarButton, useSidebarCollapsed } from "./Shell";
import { Button } from "./ui/button";
import { Kbd } from "./ui/primitives";
import { VetroMark } from "./VetroMark";
import { useHeaderChromeClass, useIntegratedTitleBar, useWindowDragProps } from "./WindowChrome";

export function EmptyState() {
  const t = useT();
  const collapsed = useSidebarCollapsed();
  const integrated = useIntegratedTitleBar();
  const drag = useWindowDragProps();
  const chromeClass = useHeaderChromeClass("main-full");
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header {...drag} className={cn("flex h-(--workspace-topbar-height) shrink-0 items-center px-(--workspace-gutter)", !collapsed && !integrated && "md:hidden", chromeClass)}>
        <MobileSidebarButton />
      </header>
      <div className="flex flex-1 flex-col items-center justify-center gap-4 px-6 text-center">
        <VetroMark className="size-9 opacity-90" />
        <div className="max-w-sm">
          <h1 className="text-base font-medium text-foreground">{t("empty.title")}</h1>
          <p className="mt-1.5 text-sm text-muted-foreground">{t("empty.body")}</p>
        </div>
        <div className="flex items-center gap-3">
          <Button size="sm" onClick={() => actions.navigate({ view: "new", projectPath: null, agentId: null })}>
            <SquarePenIcon />
            {t("sidebar.newThread")}
          </Button>
          <span className="hidden items-center gap-1.5 text-xs text-muted-foreground sm:inline-flex">
            {t("sidebar.search")} <Kbd>{isMac ? "⌘K" : "Ctrl K"}</Kbd>
          </span>
        </div>
      </div>
    </div>
  );
}
