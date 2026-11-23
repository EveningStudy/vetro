import {
  ArchiveIcon,
  ArchiveRestoreIcon,
  CopyIcon,
  CornerLeftUpIcon,
  FolderIcon,
  LockIcon,
  MoreHorizontalIcon,
  PencilIcon,
  ScrollTextIcon,
  UnplugIcon,
} from "lucide-react";
import { useEffect, useState } from "react";
import { useT } from "~/i18n";
import { tildePath } from "~/lib/format";
import { copyText } from "~/lib/utils";
import { type OpenThread, actions, useStore } from "~/state/store";
import { toast } from "~/state/toast";
import { AgentIcon } from "../AgentIcon";
import { RawLogPanel } from "../RawLogPanel";
import { MobileSidebarButton } from "../Shell";
import { Button } from "../ui/button";
import { Dialog, DialogDescription, DialogPopup, DialogTitle, Menu, MenuItem, MenuPopup, MenuSeparator, MenuTrigger, Tip } from "../ui/overlay";
import { Badge, Input, Spinner } from "../ui/primitives";
import { RpcFailure } from "~/api/protocol";
import { Composer } from "./Composer";
import { SignInCard } from "./NewThread";
import { prepare } from "./OptionPicker";
import { sourceLabel } from "./sourceLabel";
import { Timeline } from "./Timeline";

export function ThreadView({ threadId }: { threadId: string }) {
  const t = useT();
  const thread = useStore((s) => s.open[threadId]);
  const summary = useStore((s) => s.threads[threadId] ?? s.open[threadId]?.summary);
  const rawOpen = useStore((s) => s.rawPanelOpen);
  const [failure, setFailure] = useState<unknown>(null);
  const [attempt, setAttempt] = useState(0);
  const agent = useStore((s) => s.agents.find((a) => a.id === (summary?.agentId ?? threadId.split(":")[0])));

  useEffect(() => {
    setFailure(null);
    actions.retain(threadId);
    actions.openThread(threadId).catch((error: unknown) => setFailure(error ?? true));
    return () => actions.release(threadId);
  }, [threadId, attempt]);

  return (
    <div className="flex min-h-0 min-w-0 flex-1">
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        {summary ? <ThreadHeader thread={thread} threadId={threadId} /> : <BareHeader />}
        {failure && !thread ? (
          <div className="flex flex-1 flex-col items-center justify-center gap-3 px-(--workspace-gutter) text-sm text-muted-foreground">
            {needsAuth(failure) && agent ? (
              <SignInCard agent={agent} onDone={() => setAttempt((n) => n + 1)} />
            ) : (
              <>
                <span>{t("thread.notFound")}</span>
                {failure instanceof RpcFailure ? <span className="max-w-lg text-center text-xs">{failure.message}</span> : null}
                <Button size="sm" variant="outline" onClick={() => setAttempt((n) => n + 1)}>
                  {t("common.retry")}
                </Button>
              </>
            )}
          </div>
        ) : (
          <>
            <Timeline threadId={threadId} />
            <div className="shrink-0 px-(--workspace-gutter) pb-[calc(0.75rem+env(safe-area-inset-bottom))]">
              {thread ? <ThreadComposer thread={thread} /> : null}
            </div>
          </>
        )}
      </div>
      {rawOpen ? <RawLogPanel threadId={threadId} /> : null}
    </div>
  );
}

function BareHeader() {
  return (
    <header className="flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 px-(--workspace-gutter)">
      <MobileSidebarButton />
      <Spinner className="size-3.5 text-muted-foreground" />
    </header>
  );
}

function ThreadHeader({ thread, threadId }: { thread: OpenThread | undefined; threadId: string }) {
  const t = useT();
  const summary = useStore((s) => s.threads[threadId] ?? s.open[threadId]?.summary)!;
  const agent = useStore((s) => s.agents.find((a) => a.id === summary.agentId));
  const home = useStore((s) => s.app?.homeDir);
  const parent = useStore((s) => (summary.parent ? s.threads[summary.parent.threadId] : undefined));
  const rawOpen = useStore((s) => s.rawPanelOpen);
  const [renaming, setRenaming] = useState(false);
  const modelOption = thread?.options.find((option) => option.category === "model");
  const model = (modelOption && prepare(modelOption).find((choice) => choice.value === modelOption.currentValue)?.label) ?? summary.model;

  return (
    <header className="flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 px-(--workspace-gutter)">
      <MobileSidebarButton />
      <div className="flex min-w-0 flex-1 flex-col justify-center">
        <div className="flex min-w-0 items-center gap-2">
          <button
            type="button"
            onClick={() => setRenaming(true)}
            className="group/title inline-flex min-w-0 items-center gap-1 rounded-sm text-left focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring"
          >
            <h1 className="min-w-0 truncate text-sm font-medium text-foreground">{summary.title}</h1>
            <PencilIcon className="size-3 shrink-0 text-muted-foreground opacity-0 transition-opacity group-hover/title:opacity-100" />
          </button>
          {summary.live ? (
            <Tip label={t("thread.live")}>
              <span className="size-1.5 shrink-0 rounded-full bg-success" aria-label={t("thread.live")} />
            </Tip>
          ) : null}
        </div>
        <div className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
          {summary.parent ? (
            <>
              <button
                type="button"
                onClick={() => actions.navigate({ view: "thread", threadId: summary.parent!.threadId })}
                className="inline-flex min-w-0 max-w-56 shrink items-center gap-1 rounded-sm hover:text-foreground"
                title={t("thread.openParent")}
              >
                <CornerLeftUpIcon className="size-3 shrink-0" />
                {parent ? <AgentIcon agentId={parent.agentId} className="size-3" /> : null}
                <span className="truncate">{parent?.title ?? t("thread.parent")}</span>
              </button>
              <span className="text-muted-foreground/50">·</span>
            </>
          ) : null}
          <AgentIcon agentId={summary.agentId} className="size-3" />
          <span className="shrink-0">{agent?.name ?? summary.agentId}</span>
          {model ? (
            <>
              <span className="text-muted-foreground/50">·</span>
              <span className="max-w-32 shrink-0 truncate">{model}</span>
            </>
          ) : null}
          {summary.cwd ? (
            <>
              <span className="text-muted-foreground/50 max-sm:hidden">·</span>
              <span className="inline-flex min-w-0 items-center gap-1 max-sm:hidden" title={summary.cwd}>
                <FolderIcon className="size-3 shrink-0" />
                <span className="truncate font-mono text-[11px]">{tildePath(summary.cwd, home)}</span>
              </span>
            </>
          ) : null}
        </div>
      </div>
      <div className="flex shrink-0 items-center gap-1.5">
        <Badge variant="outline" className="max-sm:hidden">
          {sourceLabel(summary, agent?.name, t)}
        </Badge>
        {!summary.resumable ? (
          <Tip label={t("thread.readOnlyHint", { agent: agent?.name ?? summary.agentId })}>
            <Badge variant="warning">
              <LockIcon />
              <span className="max-sm:hidden">{t("thread.readOnly")}</span>
            </Badge>
          </Tip>
        ) : null}
        {summary.archived ? <Badge variant="secondary">{t("sidebar.archived")}</Badge> : null}
        <Tip label={t("thread.rawLog")}>
          <Button
            size="icon-sm"
            variant="ghost"
            aria-label={t("thread.rawLog")}
            aria-pressed={rawOpen}
            data-pressed={rawOpen ? "" : undefined}
            onClick={() => useStore.setState({ rawPanelOpen: !rawOpen })}
          >
            <ScrollTextIcon />
          </Button>
        </Tip>
        <Menu>
          <MenuTrigger render={<Button size="icon-sm" variant="ghost" aria-label={t("thread.more")} />}>
            <MoreHorizontalIcon />
          </MenuTrigger>
          <MenuPopup align="end" className="w-60">
            <MenuItem onClick={() => setRenaming(true)}>
              <PencilIcon />
              {t("thread.rename")}
            </MenuItem>
            <MenuItem onClick={() => void actions.updateThread(threadId, { archived: !summary.archived })}>
              {summary.archived ? <ArchiveRestoreIcon /> : <ArchiveIcon />}
              {summary.archived ? t("thread.unarchive") : t("thread.archive")}
            </MenuItem>
            <MenuItem onClick={() => useStore.setState({ rawPanelOpen: true })}>
              <ScrollTextIcon />
              {t("thread.rawLog")}
            </MenuItem>
            {parent ? (
              <MenuItem onClick={() => actions.navigate({ view: "thread", threadId: parent.id })}>
                <CornerLeftUpIcon />
                <span className="min-w-0 truncate">{t("thread.openParent")}</span>
              </MenuItem>
            ) : null}
            <MenuSeparator />
            <MenuItem onClick={() => void copyText(summary.nativeId).then(() => toast.success(t("common.copied")))}>
              <CopyIcon />
              {t("thread.copyId")}
            </MenuItem>
            <MenuItem
              disabled={!summary.live}
              onClick={() => void actions.detach(threadId).then(() => toast.info(t("thread.detached")))}
            >
              <UnplugIcon />
              {t("thread.detach")}
            </MenuItem>
          </MenuPopup>
        </Menu>
      </div>
      <RenameDialog open={renaming} onOpenChange={setRenaming} threadId={threadId} title={summary.title} />
    </header>
  );
}

function RenameDialog({ open, onOpenChange, threadId, title }: { open: boolean; onOpenChange: (open: boolean) => void; threadId: string; title: string }) {
  const t = useT();
  const [value, setValue] = useState(title);
  useEffect(() => {
    if (open) setValue(title);
  }, [open, title]);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogPopup className="max-w-md">
        <form
          className="flex flex-col gap-4 p-5"
          onSubmit={(event) => {
            event.preventDefault();
            if (value.trim()) void actions.updateThread(threadId, { title: value.trim() });
            onOpenChange(false);
          }}
        >
          <div className="flex flex-col gap-1 pe-8">
            <DialogTitle>{t("thread.renameTitle")}</DialogTitle>
            <DialogDescription>{t("thread.renameHint")}</DialogDescription>
          </div>
          <Input autoFocus value={value} onChange={(event) => setValue(event.target.value)} />
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => onOpenChange(false)}>
              {t("common.cancel")}
            </Button>
            <Button type="submit">{t("common.save")}</Button>
          </div>
        </form>
      </DialogPopup>
    </Dialog>
  );
}

function ThreadComposer({ thread }: { thread: OpenThread }) {
  const t = useT();
  const summary = thread.summary;
  const agent = useStore((s) => s.agents.find((a) => a.id === summary.agentId));
  const agentName = agent?.name ?? summary.agentId;
  const running = summary.status === "running" || summary.status === "awaiting_input";
  const [signIn, setSignIn] = useState(false);
  const needsSignIn = agent && (signIn || agent.auth.state === "required") && agent.auth.methods.length > 0;
  return (
    <>
    {needsSignIn ? <SignInCard agent={agent} onDone={() => setSignIn(false)} /> : null}
    <Composer
      draftKey={summary.id}
      agentName={agentName}
      cwd={summary.cwd}
      options={thread.options}
      commands={thread.commands}
      onOptionChange={(optionId, value) => void actions.setOption(summary.id, optionId, value)}
      onSubmit={(content) =>
        actions.send(summary.id, content).catch((error: unknown) => {
          if (needsAuth(error)) setSignIn(true);
          throw error;
        })
      }
      onStop={() => void actions.interrupt(summary.id)}
      running={running}
      midTurnInput={agent?.capabilities.midTurnInput}
      imageInput={agent?.capabilities.imageInput ?? false}
      readOnly={!summary.resumable}
      placeholder={summary.resumable ? t("composer.placeholder", { agent: agentName }) : t("composer.readOnly")}
      usage={thread.usage}
      autoFocus
    />
    </>
  );
}

function needsAuth(error: unknown) {
  return error instanceof RpcFailure && (error.code === "auth_required" || /authenticat/i.test(error.message));
}
