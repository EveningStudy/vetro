import { ChevronDownIcon, FolderIcon, FolderPlusIcon, KeyRoundIcon, LogInIcon } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { client } from "~/api/client";
import type { AgentInfo, ConfigOption, SlashCommand } from "~/api/types";
import { useT } from "~/i18n";
import { tildePath } from "~/lib/format";
import { cn } from "~/lib/utils";
import { actions, useStore } from "~/state/store";
import { errorMessage, toast } from "~/state/toast";
import { AgentIcon } from "../AgentIcon";
import { FolderBrowser } from "../FolderBrowser";
import { MobileSidebarButton } from "../Shell";
import { Button } from "../ui/button";
import { Menu, MenuGroupLabel, MenuItem, MenuPopup, MenuRadioGroup, MenuRadioItem, MenuSeparator, MenuTrigger } from "../ui/overlay";
import { Spinner } from "../ui/primitives";
import { Composer } from "./Composer";
import { composerControlClass } from "./OptionPicker";

const LAST_AGENT = "vetro:last-agent";
const LAST_PROJECT = "vetro:last-project";

function usable(agent: AgentInfo) {
  return agent.installed && !agent.error;
}

export function NewThread({ projectPath, agentId: routeAgent }: { projectPath: string | null; agentId: string | null }) {
  const t = useT();
  const agents = useStore((s) => s.agents);
  const projects = useStore((s) => s.projects);
  const home = useStore((s) => s.app?.homeDir);
  const [agentId, setAgentId] = useState<string | null>(routeAgent ?? localStorage.getItem(LAST_AGENT));
  const [cwd, setCwd] = useState<string | null>(projectPath ?? localStorage.getItem(LAST_PROJECT));
  const [baseOptions, setBaseOptions] = useState<ConfigOption[]>([]);
  const [commands, setCommands] = useState<SlashCommand[]>([]);
  const [overrides, setOverrides] = useState<Record<string, string>>({});
  const [loadingOptions, setLoadingOptions] = useState(false);
  const [browsing, setBrowsing] = useState(false);

  useEffect(() => {
    if (projectPath) setCwd(projectPath);
  }, [projectPath]);
  useEffect(() => {
    if (routeAgent) setAgentId(routeAgent);
  }, [routeAgent]);

  const agent = agents.find((a) => a.id === agentId && usable(a)) ?? agents.find((a) => usable(a) && a.auth.state !== "required") ?? agents.find(usable);
  const visibleProjects = projects.filter((p) => !p.hidden);
  const project = visibleProjects.find((p) => p.path === cwd) ?? (cwd ? null : visibleProjects[0]);
  const effectiveCwd = cwd ?? project?.path ?? null;

  useEffect(() => {
    if (!agent || agent.auth.state === "required") {
      setBaseOptions([]);
      setCommands([]);
      return;
    }
    let cancelled = false;
    setLoadingOptions(true);
    client()
      .request("agents.options", { agentId: agent.id, ...(effectiveCwd ? { cwd: effectiveCwd } : {}) })
      .then((result) => {
        if (cancelled) return;
        setBaseOptions(result.options);
        setCommands(result.commands);
      })
      .catch(() => {
        if (!cancelled) setBaseOptions([]);
      })
      .finally(() => !cancelled && setLoadingOptions(false));
    setOverrides({});
    return () => {
      cancelled = true;
    };
  }, [agent?.id, agent?.auth.state, effectiveCwd]);

  const options = useMemo(
    () => baseOptions.map((option) => (overrides[option.id] ? { ...option, currentValue: overrides[option.id]! } : option)),
    [baseOptions, overrides],
  );

  const chooseProject = async (path: string) => {
    try {
      const added = await actions.addProject(path);
      setCwd(added.path);
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  const needsSignIn = agent?.auth.state === "required";
  const agentName = agent?.name ?? "";

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <header className="flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 px-(--workspace-gutter) md:hidden">
        <MobileSidebarButton />
      </header>
      <div className="flex min-h-0 flex-1 flex-col justify-center overflow-y-auto px-(--workspace-gutter) pb-[calc(1rem+env(safe-area-inset-bottom))] max-sm:justify-end">
        <div className="mx-auto mb-6 w-full max-w-(--chat-content-max-width) text-center max-sm:mb-4 max-sm:text-left">
          <h1 className="text-2xl font-semibold tracking-tight text-foreground max-sm:text-xl">{t("new.title")}</h1>
          <p className="mt-1.5 text-sm text-muted-foreground">{t("new.subtitle")}</p>
        </div>
        {needsSignIn && agent ? <SignInCard agent={agent} /> : null}
        <Composer
          draftKey="new"
          agentName={agentName}
          cwd={effectiveCwd}
          options={options}
          commands={commands}
          onOptionChange={(optionId, value) => setOverrides((current) => ({ ...current, [optionId]: value }))}
          onSubmit={async (content) => {
            if (!agent || !effectiveCwd) return;
            const changed: Record<string, string> = {};
            for (const option of baseOptions) {
              const value = overrides[option.id];
              if (value && value !== option.currentValue) changed[option.id] = value;
            }
            localStorage.setItem(LAST_AGENT, agent.id);
            localStorage.setItem(LAST_PROJECT, effectiveCwd);
            await actions.start(agent.id, effectiveCwd, content, changed);
          }}
          imageInput={agent?.capabilities.imageInput ?? false}
          readOnly={!agent}
          sendBlocked={needsSignIn ? t("composer.needsSignIn") : !effectiveCwd ? t("composer.needProject") : null}
          placeholder={t("composer.placeholderNew")}
          autoFocus
          leading={
            <>
              <AgentPicker agents={agents} current={agent} onChange={setAgentId} />
              {loadingOptions ? <Spinner className="mx-1.5 size-3.5 text-muted-foreground" /> : null}
            </>
          }
          below={
            <div className="flex items-center gap-1 px-1 pt-2 text-muted-foreground">
              <Menu>
                <MenuTrigger className={cn(composerControlClass, "max-w-full font-normal text-muted-foreground")}>
                  <FolderIcon />
                  <span className="min-w-0 truncate">{project?.name ?? (cwd ? tildePath(cwd, home) : t("composer.chooseProject"))}</span>
                  {effectiveCwd ? <span className="hidden min-w-0 truncate font-mono text-[11px] text-muted-foreground/70 sm:inline">{tildePath(effectiveCwd, home)}</span> : null}
                  <ChevronDownIcon className="size-3 opacity-70" />
                </MenuTrigger>
                <MenuPopup align="start" className="w-80">
                  <MenuGroupLabel>{t("composer.project")}</MenuGroupLabel>
                  <MenuRadioGroup value={effectiveCwd ?? ""} onValueChange={(value) => setCwd(String(value))}>
                    {visibleProjects.map((p) => (
                      <MenuRadioItem key={p.path} value={p.path} disabled={!p.exists}>
                        <FolderIcon />
                        <span className="flex min-w-0 flex-col">
                          <span className="truncate">{p.name}</span>
                          <span className="truncate font-mono text-[11px] text-muted-foreground">{tildePath(p.path, home)}</span>
                        </span>
                      </MenuRadioItem>
                    ))}
                  </MenuRadioGroup>
                  <MenuSeparator />
                  <MenuItem onClick={() => setBrowsing(true)}>
                    <FolderPlusIcon />
                    {t("composer.addFolder")}
                  </MenuItem>
                </MenuPopup>
              </Menu>
            </div>
          }
        />
      </div>
      <FolderBrowser open={browsing} onOpenChange={setBrowsing} onChoose={(path) => void chooseProject(path)} />
    </div>
  );
}

function AgentPicker({ agents, current, onChange }: { agents: AgentInfo[]; current: AgentInfo | undefined; onChange: (id: string) => void }) {
  const t = useT();
  return (
    <Menu>
      <MenuTrigger className={composerControlClass} aria-label={t("composer.agent")}>
        {current ? <AgentIcon agentId={current.id} className="size-4" /> : null}
        <span className="max-w-32 truncate">{current?.name ?? t("composer.agent")}</span>
        <ChevronDownIcon className="-me-1 size-3 opacity-70" />
      </MenuTrigger>
      <MenuPopup side="top" align="start" className="w-72">
        <MenuGroupLabel>{t("composer.agent")}</MenuGroupLabel>
        <MenuRadioGroup value={current?.id ?? ""} onValueChange={(value) => onChange(String(value))}>
          {agents.map((agent) => {
            const unavailable = !agent.installed || !!agent.error;
            return (
              <MenuRadioItem key={agent.id} value={agent.id} disabled={unavailable} className="items-start py-1.5">
                <AgentIcon agentId={agent.id} className="mt-0.5" />
                <span className="flex min-w-0 flex-col">
                  <span className="flex items-center gap-1.5">
                    <span className="truncate">{agent.name}</span>
                    {agent.version ? <span className="text-xs text-muted-foreground tabular-nums">{agent.version}</span> : null}
                  </span>
                  {unavailable ? (
                    <span className="text-xs text-muted-foreground">{agent.error ?? t("composer.unavailable")}</span>
                  ) : agent.auth.state === "required" ? (
                    <span className="inline-flex items-center gap-1 text-xs text-warning-foreground">
                      <KeyRoundIcon className="size-3" />
                      {t("composer.needsSignIn")}
                    </span>
                  ) : agent.auth.account ? (
                    <span className="text-xs text-muted-foreground">{agent.auth.account}</span>
                  ) : null}
                </span>
              </MenuRadioItem>
            );
          })}
        </MenuRadioGroup>
      </MenuPopup>
    </Menu>
  );
}

export function SignInCard({ agent }: { agent: AgentInfo }) {
  const t = useT();
  const [pending, setPending] = useState<string | null>(null);
  return (
    <div className="mx-auto mb-3 w-full max-w-(--chat-content-max-width) rounded-xl border border-warning/40 bg-warning-surface/60 px-4 py-3">
      <div className="flex items-start gap-3">
        <AgentIcon agentId={agent.id} className="mt-0.5 size-5" />
        <div className="min-w-0 flex-1">
          <div className="text-sm font-medium">{t("composer.needsSignIn")}</div>
          <p className="mt-0.5 text-xs text-muted-foreground">{t("composer.signInHint", { agent: agent.name })}</p>
          <div className="mt-2.5 flex flex-wrap gap-2">
            {agent.auth.methods.map((method, index) => (
              <Button
                key={method.id}
                size="sm"
                variant={index === 0 ? "default" : "outline"}
                disabled={pending !== null}
                title={method.description ?? undefined}
                onClick={() => {
                  setPending(method.id);
                  client()
                    .request("agents.authenticate", { agentId: agent.id, methodId: method.id })
                    .then(() => client().request("agents.refresh", {}))
                    .then((result) => useStore.setState({ agents: result.agents }))
                    .catch((error) => toast.error(errorMessage(error)))
                    .finally(() => setPending(null));
                }}
              >
                {pending === method.id ? <Spinner className="size-3.5" /> : <LogInIcon />}
                {pending === method.id ? t("composer.signingIn") : method.name}
              </Button>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
