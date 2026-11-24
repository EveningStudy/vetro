import { ExternalLinkIcon, XIcon } from "lucide-react";
import { useEffect } from "react";
import { useT } from "~/i18n";
import { actions, useStore } from "~/state/store";
import { AgentIcon } from "./AgentIcon";
import { Timeline } from "./chat/Timeline";
import { RawLogPanel } from "./RawLogPanel";
import { Button } from "./ui/button";
import { Tip } from "./ui/overlay";
import { Spinner } from "./ui/primitives";

/** The single right-side inspector slot. Each drawer kind renders its own header and body. */
export function RightDrawer({ threadId }: { threadId: string }) {
  const drawer = useStore((s) => s.rightDrawer);
  if (!drawer) return null;
  return (
    <>
      <div className="fixed inset-0 z-40 bg-black/40 md:hidden" aria-hidden onClick={actions.closeDrawer} />
      <aside
        data-right-drawer={drawer.kind}
        className="fixed inset-y-0 right-0 z-40 flex min-h-0 w-[min(100vw,32rem)] flex-col border-l bg-background pt-[env(safe-area-inset-top)] shadow-xl md:static md:z-auto md:w-[min(30rem,44vw)] md:shrink-0 md:pt-0 md:shadow-none"
      >
        {drawer.kind === "raw" ? <RawLogPanel threadId={threadId} /> : <ChildSessionPeek key={drawer.threadId} threadId={drawer.threadId} />}
      </aside>
    </>
  );
}

function ChildSessionPeek({ threadId }: { threadId: string }) {
  const t = useT();
  const summary = useStore((s) => s.threads[threadId] ?? s.open[threadId]?.summary);
  const agentId = summary?.agentId ?? threadId.split(":")[0]!;

  useEffect(() => {
    actions.retain(threadId);
    return () => actions.release(threadId);
  }, [threadId]);

  return (
    <>
      <div className="flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 border-b ps-4 pe-2">
        <AgentIcon agentId={agentId} className="size-4" />
        <span className="min-w-0 flex-1 truncate text-sm font-medium" title={summary?.title}>
          {summary?.title ?? t("drawer.child")}
        </span>
        {summary?.status === "running" ? <Spinner className="size-3.5 shrink-0 text-info-foreground" /> : null}
        <Button
          size="xs"
          variant="ghost-muted"
          onClick={() => {
            actions.closeDrawer();
            actions.navigate({ view: "thread", threadId });
          }}
        >
          <ExternalLinkIcon />
          <span className="max-sm:hidden">{t("drawer.openFull")}</span>
        </Button>
        <Tip label={t("common.close")}>
          <Button size="icon-sm" variant="ghost" aria-label={t("common.close")} onClick={actions.closeDrawer}>
            <XIcon />
          </Button>
        </Tip>
      </div>
      <Timeline threadId={threadId} />
    </>
  );
}
