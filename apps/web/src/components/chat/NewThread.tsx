import { ChevronDownIcon, FolderGit2Icon, KeyRoundIcon, SlidersHorizontalIcon } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { client } from "~/api/client";
import type { AgentInfo, ConfigOption, SlashCommand, WorktreeInfo, WorktreesResult } from "~/api/types";
import { useT } from "~/i18n";
import { basename, shortVersion, tildePath } from "~/lib/format";
import { cn } from "~/lib/utils";
import { getPrefs, rememberAgentOption, resolveSavedAgentOptions, savePrefs, usePrefs } from "~/state/prefs";
import { actions, useStore } from "~/state/store";
import { errorMessage, toast } from "~/state/toast";
import { AgentIcon } from "../AgentIcon";
import { SignInMethods } from "../SignIn";
import { FolderBrowser } from "../FolderBrowser";
import { MobileSidebarButton, useSidebarCollapsed } from "../Shell";
import { isQuickChatPath, isScratchOrTempPath } from "../sidebar/Sidebar";
import { Menu, MenuGroupLabel, MenuPopup, MenuRadioGroup, MenuRadioItem, MenuTrigger } from "../ui/overlay";
import { Spinner } from "../ui/primitives";
import { useHeaderChromeClass, useIntegratedTitleBar, useWindowDragProps } from "../WindowChrome";
import { Composer } from "./Composer";
import { ProjectPicker } from "./ProjectPicker";
import { composerControlClass } from "./OptionPicker";


function usable(agent: AgentInfo) {
  return agent.installed && !agent.error;
}

export function NewThread({ projectPath, agentId: routeAgent }: { projectPath: string | null; agentId: string | null }) {
  const t = useT();
  const agents = useStore((s) => s.agents);
  const projects = useStore((s) => s.projects);
  const home = useStore((s) => s.app?.homeDir);
  const dataDir = useStore((s) => s.app?.dataDir);
  const [agentId, setAgentId] = useState<string | null>(routeAgent ?? getPrefs().lastAgent ?? null);
  const [cwd, setCwd] = useState<string | null>(projectPath);
  const [baseOptions, setBaseOptions] = useState<ConfigOption[]>([]);
  const [commands, setCommands] = useState<SlashCommand[]>([]);
  const [overrides, setOverrides] = useState<Record<string, string>>({});
  const [defaults, setDefaults] = useState<Record<string, string>>({});
  const [needsSession, setNeedsSession] = useState(false);
  const [loadingOptions, setLoadingOptions] = useState(false);
  const [browsing, setBrowsing] = useState(false);
  const bypassMemoryScope = usePrefs((s) => s.bypassMemoryScope);
  const reqSeqRef = useRef(0);

  useEffect(() => {
    setCwd(projectPath);
  }, [projectPath]);
  useEffect(() => {
    if (routeAgent) setAgentId(routeAgent);
  }, [routeAgent]);

  const agent = agents.find((a) => a.id === agentId && usable(a)) ?? agents.find((a) => usable(a) && a.auth.state !== "required") ?? agents.find(usable);
  const lastProject = getPrefs().lastProject ?? null;
  const visibleProjects = useMemo(() => {
    const clean = projects.filter((p) => !p.hidden && !isScratchOrTempPath(p.path) && !isQuickChatPath(p.path, dataDir));
    if (!lastProject) return clean;
    const index = clean.findIndex((p) => p.path === lastProject);
    if (index <= 0) return clean;
    return [clean[index]!, ...clean.slice(0, index), ...clean.slice(index + 1)];
  }, [projects, dataDir, lastProject]);
  const effectiveCwd = cwd;
  const realProjectCwd = effectiveCwd && !isScratchOrTempPath(effectiveCwd) && !isQuickChatPath(effectiveCwd, dataDir) ? effectiveCwd : null;
  const worktreeCwd = effectiveCwd && !isQuickChatPath(effectiveCwd, dataDir) ? effectiveCwd : null;
  const worktrees = useStore((s) => {
    if (!worktreeCwd) return undefined;
    const direct = s.worktrees[worktreeCwd];
    if (direct) return direct;
    return Object.values(s.worktrees).find((result) => result.worktrees.some((wt) => samePath(wt.path, worktreeCwd)));
  });

  useEffect(() => {
    if (worktreeCwd) void actions.refreshWorktrees(worktreeCwd);
  }, [worktreeCwd]);

  const hydrateLoadedOptions = (
    targetAgentId: string,
    targetCwd: string | null,
    targetRealCwd: string | null,
    result: { options: ConfigOption[]; commands: SlashCommand[]; needsSession?: boolean },
    seq: number,
  ) => {
    setBaseOptions(result.options);
    const baseDefaults = Object.fromEntries(result.options.map((option) => [option.id, option.currentValue]));
    setDefaults(baseDefaults);
    setCommands(result.commands);
    setNeedsSession(!!result.needsSession);
    const saved = resolveSavedAgentOptions(targetAgentId, targetRealCwd, result.options, bypassMemoryScope);
    setOverrides(saved);

    const modelOption = result.options.find((option) => option.category === "model");
    const savedModel = modelOption ? saved[modelOption.id] : undefined;
    if (!modelOption || !savedModel || savedModel === modelOption.currentValue) return;

    client()
      .request("agents.options", {
        agentId: targetAgentId,
        ...(targetCwd ? { cwd: targetCwd } : {}),
        selected: { [modelOption.id]: savedModel },
      })
      .then((modelResult) => {
        if (reqSeqRef.current !== seq || modelResult.options.length === 0) return;
        setBaseOptions(modelResult.options);
        setDefaults((current) => {
          const nextDefaults = { ...current };
          for (const option of modelResult.options) {
            if (option.category !== "model") nextDefaults[option.id] = option.currentValue;
          }
          return nextDefaults;
        });
        const mergedSaved = resolveSavedAgentOptions(targetAgentId, targetRealCwd, modelResult.options, bypassMemoryScope);
        setOverrides({ ...mergedSaved, [modelOption.id]: savedModel });
      })
      .catch(() => {});
  };

  useEffect(() => {
    const seq = ++reqSeqRef.current;
    if (!agent || agent.auth.state === "required") {
      setBaseOptions([]);
      setDefaults({});
      setCommands([]);
      setOverrides({});
      setNeedsSession(false);
      return;
    }
    setLoadingOptions(true);
    setOverrides({});
    client()
      .request("agents.options", { agentId: agent.id, ...(effectiveCwd ? { cwd: effectiveCwd } : {}) })
      .then((result) => {
        if (reqSeqRef.current !== seq) return;
        hydrateLoadedOptions(agent.id, effectiveCwd, realProjectCwd, result, seq);
      })
      .catch(() => {
        if (reqSeqRef.current === seq) setBaseOptions([]);
      })
      .finally(() => {
        if (reqSeqRef.current === seq) setLoadingOptions(false);
      });
  }, [agent?.id, agent?.auth.state, effectiveCwd, realProjectCwd, bypassMemoryScope]);

  const options = useMemo(
    () =>
      baseOptions.map((option) => {
        const value = overrides[option.id];
        return value && option.choices.some((choice) => choice.value === value) ? { ...option, currentValue: value } : option;
      }),
    [baseOptions, overrides],
  );

  const prepareOptions = () => {
    if (!agent) return;
    const seq = ++reqSeqRef.current;
    setLoadingOptions(true);
    client()
      .request("agents.options", { agentId: agent.id, ...(effectiveCwd ? { cwd: effectiveCwd } : {}), prepare: true })
      .then((result) => {
        if (reqSeqRef.current !== seq) return;
        hydrateLoadedOptions(agent.id, effectiveCwd, realProjectCwd, result, seq);
      })
      .catch((error) => toast.error(errorMessage(error)))
      .finally(() => {
        if (reqSeqRef.current === seq) setLoadingOptions(false);
      });
  };

  const changeOption = (optionId: string, value: string) => {
    const next = { ...overrides, [optionId]: value };
    setOverrides(next);
    const changed = baseOptions.find((option) => option.id === optionId);
    if (!agent || !changed) return;
    const modelOption = options.find((option) => option.category === "model");
    const activeModel = changed.category === "model" ? value : modelOption?.currentValue;
    rememberAgentOption(agent.id, realProjectCwd, changed, value, defaults[optionId], activeModel, bypassMemoryScope);
    if (changed.category !== "model") return;
    const seq = ++reqSeqRef.current;
    setLoadingOptions(true);
    client()
      .request("agents.options", {
        agentId: agent.id,
        ...(effectiveCwd ? { cwd: effectiveCwd } : {}),
        selected: { [optionId]: value },
        prepare: true,
      })
      .then((result) => {
        if (reqSeqRef.current !== seq) return;
        setBaseOptions(result.options);
        setDefaults((current) => {
          const nextDefaults = { ...current };
          for (const option of result.options) {
            if (option.category !== "model" || !(option.id in nextDefaults)) nextDefaults[option.id] = option.currentValue;
          }
          return nextDefaults;
        });
        const rememberedForModel = resolveSavedAgentOptions(agent.id, realProjectCwd, result.options, bypassMemoryScope);
        setOverrides((current) => {
          const kept: Record<string, string> = {};
          for (const [id, chosen] of Object.entries(current)) {
            const option = result.options.find((o) => o.id === id);
            if (option?.choices.some((choice) => choice.value === chosen)) kept[id] = chosen;
          }
          for (const option of result.options) {
            if (option.category === "thought_level") {
              const savedLevel = rememberedForModel[option.id];
              if (savedLevel) kept[option.id] = savedLevel;
              else delete kept[option.id];
            }
          }
          kept[optionId] = value;
          return kept;
        });
      })
      .catch((error) => toast.error(errorMessage(error)))
      .finally(() => {
        if (reqSeqRef.current === seq) setLoadingOptions(false);
      });
  };

  const chooseProject = async (path: string) => {
    try {
      const added = await actions.addProject(path);
      setCwd(added.path);
    } catch (error) {
      toast.error(errorMessage(error));
    }
  };

  const needsSignIn = agent?.auth.state === "required";
  const remoteClient = useStore((s) => s.app?.remoteClient ?? false);
  const unknownFolder = remoteClient && !!effectiveCwd && !projects.some((p) => p.path === effectiveCwd);
  const agentName = agent?.name ?? "";
  const sidebarCollapsed = useSidebarCollapsed();
  const integratedTitleBar = useIntegratedTitleBar();
  const drag = useWindowDragProps();
  const chromeClass = useHeaderChromeClass("main-full");

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <header
        {...drag}
        className={cn("flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 px-(--workspace-gutter)", !sidebarCollapsed && !integratedTitleBar && "md:hidden", chromeClass)}
      >
        <MobileSidebarButton />
      </header>
      <div className="flex min-h-0 flex-1 flex-col justify-center overflow-y-auto px-(--workspace-gutter) pb-[calc(1rem+env(safe-area-inset-bottom))] max-sm:justify-end">
        <div className="mx-auto mb-6 w-full max-w-(--chat-content-max-width) text-center max-sm:mb-4 max-sm:text-left">
          <h1 className="text-2xl font-semibold tracking-tight text-foreground max-sm:text-xl">{t("new.title")}</h1>
        </div>
        {needsSignIn && agent ? <SignInCard key={agent.id} agent={agent} /> : null}
        <Composer
          draftKey="new"
          agentName={agentName}
          cwd={effectiveCwd}
          options={options}
          defaultOptions={defaults}
          commands={commands}
          onOptionChange={changeOption}
          onSubmit={async (content) => {
            if (!agent) return;
            const changed: Record<string, string> = {};
            for (const [id, value] of Object.entries(overrides)) {
              const option = options.find((o) => o.id === id);
              if (option?.currentValue === value && value !== defaults[id]) changed[id] = value;
            }
            savePrefs({ lastAgent: agent.id, ...(realProjectCwd ? { lastProject: realProjectCwd } : {}) });
            await actions.start(agent.id, effectiveCwd ?? "", content, changed);
          }}
          imageInput={agent?.capabilities.imageInput ?? false}
          agent={agent}
          readOnly={!agent}
          sendBlocked={needsSignIn ? t("composer.needsSignIn") : unknownFolder ? t("remote.knownProject") : null}
          placeholder={t("composer.placeholderNew", { agent: agentName || t("composer.agent") })}
          autoFocus
          leading={
            <>
              <AgentPicker agents={agents} current={agent} onChange={setAgentId} />
              {loadingOptions ? <Spinner className="mx-1.5 size-3.5 text-muted-foreground" /> : null}
              {needsSession && !loadingOptions && agent ? (
                <button
                  type="button"
                  onClick={prepareOptions}
                  className={composerControlClass}
                >
                  <SlidersHorizontalIcon />
                  <span>{t("composer.loadOptions")}</span>
                </button>
              ) : null}
            </>
          }
          below={
            <div className="flex min-w-0 items-center gap-1">
              <ProjectPicker projects={visibleProjects} value={effectiveCwd} home={home} onChange={setCwd} onAddFolder={() => setBrowsing(true)} />
              {effectiveCwd && worktrees?.isRepo && worktrees.worktrees.length > 1 ? (
                <WorktreePicker result={worktrees} cwd={effectiveCwd} home={home} onChange={setCwd} />
              ) : null}
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
      <MenuTrigger className={cn(composerControlClass, "max-w-[8rem]")} aria-label={t("composer.agent")}>
        {current ? <AgentIcon agentId={current.id} className="size-4 shrink-0" /> : null}
        <span className="min-w-0 truncate">{current?.name ?? t("composer.agent")}</span>
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
                    {agent.version ? <span className="truncate text-xs text-muted-foreground tabular-nums" title={agent.version}>{shortVersion(agent.version)}</span> : null}
                  </span>
                  {unavailable ? (
                    <span className="text-xs text-muted-foreground">{agent.error ?? t("composer.unavailable")}</span>
                  ) : agent.auth.state === "required" ? (
                    <span className="inline-flex items-center gap-1 text-xs text-warning-foreground">
                      <KeyRoundIcon className="size-3" />
                      {t("composer.needsSignIn")}
                    </span>
                  ) : agent.auth.provider ? (
                    <span className="truncate text-xs text-muted-foreground">
                      {t("settings.via", { provider: agent.auth.provider })}
                      {agent.auth.endpoint ? <span className="font-mono text-[11px]"> · {agent.auth.endpoint}</span> : null}
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

function samePath(a: string, b: string) {
  const normalize = (path: string) => path.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
  return normalize(a) === normalize(b);
}

function WorktreePicker({
  result,
  cwd,
  home,
  onChange,
}: {
  result: WorktreesResult;
  cwd: string;
  home: string | undefined;
  onChange: (path: string) => void;
}) {
  const t = useT();
  const list = result.worktrees;
  const current = list.find((wt) => samePath(wt.path, cwd)) ?? list.find((wt) => wt.isCurrent);
  const label = (wt: WorktreeInfo) => wt.branch ?? (wt.detached && wt.head ? wt.head.slice(0, 7) : basename(wt.path));
  return (
    <Menu>
      <MenuTrigger className={cn(composerControlClass, "max-w-[12rem]")} aria-label={t("worktree.switch")}>
        <FolderGit2Icon className="shrink-0" />
        <span className="min-w-0 truncate">{current ? label(current) : t("worktree.badge")}</span>
        {current && !current.isMain ? <span className="shrink-0 text-2xs text-muted-foreground">{t("worktree.badge")}</span> : null}
        <ChevronDownIcon className="-me-1 size-3 opacity-70" />
      </MenuTrigger>
      <MenuPopup side="top" align="start" className="w-80">
        <MenuGroupLabel>{t("worktree.switch")}</MenuGroupLabel>
        <MenuRadioGroup
          value={current?.path ?? ""}
          onValueChange={(value) => {
            const path = String(value);
            if (path && !samePath(path, cwd)) onChange(path);
          }}
        >
          {list.map((wt) => (
            <MenuRadioItem key={wt.path} value={wt.path} className="items-start py-1.5">
              <span className="flex min-w-0 flex-col">
                <span className="flex items-center gap-1.5">
                  <span className="truncate font-mono text-xs">{label(wt)}</span>
                  {wt.isMain ? <span className="shrink-0 text-2xs text-muted-foreground">{t("worktree.main")}</span> : null}
                  {wt.detached ? <span className="shrink-0 text-2xs text-muted-foreground">{t("worktree.detached")}</span> : null}
                </span>
                <span className="truncate font-mono text-[11px] text-muted-foreground" title={wt.path}>
                  {tildePath(wt.path, home)}
                </span>
              </span>
            </MenuRadioItem>
          ))}
        </MenuRadioGroup>
      </MenuPopup>
    </Menu>
  );
}

export function SignInCard({ agent, onDone }: { agent: AgentInfo; onDone?: () => void }) {
  const t = useT();
  return (
    <div className="mx-auto mb-3 w-full max-w-(--chat-content-max-width) rounded-xl border border-warning/40 bg-warning-surface/60 px-4 py-3">
      <div className="flex items-start gap-3">
        <AgentIcon agentId={agent.id} className="mt-0.5 size-5" />
        <div className="min-w-0 flex-1">
          <div className="text-sm font-medium">{t("composer.needsSignIn")}</div>
          <p className="mt-0.5 text-xs text-muted-foreground">{t("composer.signInHint", { agent: agent.name })}</p>
          <div className="mt-2.5">
            <SignInMethods agent={agent} onDone={onDone} />
          </div>
        </div>
      </div>
    </div>
  );
}
