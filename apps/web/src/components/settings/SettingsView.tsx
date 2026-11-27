import {
  BellIcon,
  BotIcon,
  ChevronDownIcon,
  DownloadIcon,
  GlobeIcon,
  InfoIcon,
  KeyRoundIcon,
  MessageSquareIcon,
  MicIcon,
  MonitorIcon,
  MoonIcon,
  PaletteIcon,
  RefreshCwIcon,
  SlidersHorizontalIcon,
  SmartphoneIcon,
  SunIcon,
  Trash2Icon,
} from "lucide-react";
import { type ReactNode, useEffect, useId, useRef, useState } from "react";
import { client } from "~/api/client";
import type {
  AgentInfo,
  AgentSettings,
  BadgeClearMode,
  BadgeScope,
  NotificationMode,
  NotificationSettings,
  Pairing,
  Settings,
} from "~/api/types";
import type { RemoteSetParams } from "~/api/bindings/RemoteSetParams";
import { htmlLangTag, LOCALE_LABELS, LOCALES, useT } from "~/i18n";
import { useDesktopChrome } from "~/lib/desktopChrome";
import { relativeTime, shortVersion } from "~/lib/format";
import {
  hasPopup,
  hasSound,
  playNotificationChime,
  resolvedNotificationSettings,
  unlockNotificationAudio,
} from "~/lib/notifications";
import { cn, copyText } from "~/lib/utils";
import {
  type BypassMemoryScope,
  type ChatWidth,
  type FontScale,
  type FontStyle,
  type LanguagePref,
  type SidebarOrder,
  type ThemePref,
  type UsageDisplay,
  setBypassMemoryScope,
  setChatWidth,
  setComposerShowContext,
  setComposerShowLimits,
  setFontScale,
  setFontStyle,
  setHideDisabledAgentHistory,
  setLanguage,
  setPaperGrain,
  setSidebarActivityDays,
  setSidebarOrder,
  setTheme,
  setUsageDisplay,
  usePrefs,
} from "~/state/prefs";
import { type SettingsSection, actions, useStore } from "~/state/store";
import { errorMessage, toast } from "~/state/toast";
import { AgentIcon } from "../AgentIcon";
import { BUILTIN_AGENT_IDS, CustomAgents } from "./CustomAgents";
import { SignInMethods } from "../SignIn";
import { voiceModelDescription, voiceModelLabel } from "../chat/VoiceButton";
import { limitBarTone, limitTone, resetLabel, shortLimitLabel, shownPercent, snapshotAge } from "../chat/RateLimits";
import { QrCodeSvg } from "../QrCode";
import { MobileSidebarButton } from "../Shell";
import { useHeaderChromeClass, useWindowDragProps } from "../WindowChrome";
import { ThemeGallery } from "./ThemeStudio";
import { useNow } from "../hooks";
import { Button } from "../ui/button";
import {
  Dialog,
  DialogDescription,
  DialogPopup,
  DialogTitle,
  Menu,
  MenuPopup,
  MenuRadioGroup,
  MenuRadioItem,
  MenuTrigger,
} from "../ui/overlay";
import { Badge, Input, Label, Spinner, Switch, Textarea } from "../ui/primitives";
import { Segmented } from "../ui/segmented";

const SECTIONS: { id: SettingsSection; icon: typeof BotIcon }[] = [
  { id: "general", icon: SlidersHorizontalIcon },
  { id: "threads", icon: MessageSquareIcon },
  { id: "appearance", icon: PaletteIcon },
  { id: "notifications", icon: BellIcon },
  { id: "agents", icon: BotIcon },
  { id: "voice", icon: MicIcon },
  { id: "remote", icon: GlobeIcon },
  { id: "about", icon: InfoIcon },
];

export function SettingsView({ section }: { section: SettingsSection }) {
  const t = useT();
  const drag = useWindowDragProps();
  const chromeClass = useHeaderChromeClass("main-full");
  const updateAvailable = useStore((s) => !!s.updateCheck?.available);
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header {...drag} className={cn("flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 px-(--workspace-gutter)", chromeClass)}>
        <MobileSidebarButton />
        <h1 className="text-sm font-medium">{t("settings.title")}</h1>
      </header>
      <div className="flex min-h-0 flex-1 flex-col md:flex-row">
        <nav className="flex shrink-0 gap-1 overflow-x-auto border-b px-(--workspace-gutter) pb-2 [scrollbar-width:none] md:w-52 md:flex-col md:border-b-0 md:px-3 md:pb-0">
          {SECTIONS.map(({ id, icon: Icon }) => (
            <button
              key={id}
              type="button"
              aria-current={section === id ? "page" : undefined}
              onClick={() => actions.navigate({ view: "settings", section: id }, true)}
              className={cn(
                "flex h-8 shrink-0 items-center gap-2.5 rounded-md px-2.5 text-sm font-medium text-foreground/80 hover:bg-accent hover:text-foreground",
                section === id && "bg-accent text-foreground",
              )}
            >
              <Icon className="size-4 text-muted-foreground" />
              <span className="truncate">{t(`settings.${id}`)}</span>
              {id === "about" && updateAvailable ? (
                <span className="ms-auto size-2 shrink-0 rounded-full bg-primary" />
              ) : null}
            </button>
          ))}
        </nav>
        <div className="min-h-0 flex-1 overflow-y-auto">
          <div className="mx-auto w-full max-w-2xl px-(--workspace-gutter) pt-2 pb-16 md:pt-0">
            {section === "general" ? <GeneralSection /> : null}
            {section === "threads" ? <ThreadsSection /> : null}
            {section === "appearance" ? <AppearanceSection /> : null}
            {section === "notifications" ? <NotificationsSection /> : null}
            {section === "agents" ? <AgentsSection /> : null}
            {section === "voice" ? <VoiceSection /> : null}
            {section === "remote" ? <RemoteSection /> : null}
            {section === "about" ? <AboutSection /> : null}
          </div>
        </div>
      </div>
    </div>
  );
}

function SectionTitle({ title, action }: { title: string; action?: ReactNode }) {
  return (
    <div className="mb-3 flex items-center justify-between gap-3">
      <h2 className="text-base font-semibold">{title}</h2>
      {action}
    </div>
  );
}

function Group({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn("divide-y divide-border/50 overflow-hidden rounded-xl border border-border/60 bg-card/40 shadow-xs/5", className)}>{children}</div>;
}

function Row({ label, hint, labelId, children }: { label: ReactNode; hint?: ReactNode; labelId?: string; children?: ReactNode }) {
  return (
    <div className="@container">
      <div className="flex flex-col gap-3 px-4 py-3 @min-[32rem]:grid @min-[32rem]:grid-cols-[minmax(0,1fr)_auto] @min-[32rem]:items-center @min-[32rem]:gap-8">
        <div className="min-w-0">
          <div id={labelId} className="text-sm font-medium text-foreground">{label}</div>
          {hint ? <div className="mt-0.5 max-w-2xl text-pretty text-xs text-muted-foreground">{hint}</div> : null}
        </div>
        {children ? <div className="flex shrink-0 items-center gap-2 @max-[32rem]:self-end">{children}</div> : null}
      </div>
    </div>
  );
}

function countdown(ms: number) {
  const seconds = Math.max(0, Math.round(ms / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

export function displayPath(path: string | null) {
  return path?.startsWith("\\\\?\\") ? path.slice(4) : (path ?? "");
}

function useSettings() {
  const settings = useStore((s) => s.settings);
  useEffect(() => {
    client()
      .request("settings.get", {})
      .then(actions.setSettings)
      .catch((error) => toast.error(errorMessage(error)));
  }, []);
  const update = async (patch: unknown) => {
    try {
      const next = await client().request("settings.update", { patch: patch as never });
      actions.setSettings(next);
      return next;
    } catch (error) {
      toast.error(errorMessage(error));
      return null;
    }
  };
  return { settings, update };
}

function AgentsSection() {
  const t = useT();
  const agents = useStore((s) => s.agents);
  const { settings, update } = useSettings();
  const [refreshing, setRefreshing] = useState(false);
  const [showMoreAgents, setShowMoreAgents] = useState(false);
  const remoteClient = useStore((s) => s.app?.remoteClient ?? false);
  const isCustom = (id: string) => !BUILTIN_AGENT_IDS.includes(id) && (settings?.customAcpAgents.some((c) => c.id === id) ?? false);
  const listed = agents.filter((agent, index) => agents.findIndex((other) => other.id === agent.id) === index);
  const isPrimaryAgent = (agent: AgentInfo) =>
    agent.installed || isCustom(agent.id) || Boolean(settings?.agents[agent.id]?.binaryPath);
  const installedAgents = listed.filter(isPrimaryAgent);
  const uninstalledAgents = listed.filter((agent) => !isPrimaryAgent(agent));
  const expandedMore = showMoreAgents || installedAgents.length === 0;
  return (
    <section>
      <SectionTitle
        title={t("settings.agents")}
        action={
          <Button
            size="sm"
            variant="outline"
            disabled={refreshing}
            onClick={() => {
              setRefreshing(true);
              client()
                .request("agents.refresh", {})
                .then((result) => useStore.setState({ agents: result.agents }))
                .catch((error) => toast.error(errorMessage(error)))
                .finally(() => setRefreshing(false));
            }}
          >
            {refreshing ? <Spinner className="size-3.5" /> : <RefreshCwIcon />}
            {t("settings.refreshAgents")}
          </Button>
        }
      />
      {remoteClient ? <p className="mb-3 text-xs text-muted-foreground">{t("settings.remoteReadOnly")}</p> : null}
      <div className="flex flex-col gap-3">
        {installedAgents.map((agent) => (
          <AgentCard key={agent.id} agent={agent} settings={settings?.agents[agent.id]} custom={isCustom(agent.id)} onSave={update} readOnly={remoteClient} />
        ))}
      </div>
      {uninstalledAgents.length > 0 ? (
        <div className="mt-4">
          <button
            type="button"
            aria-expanded={expandedMore}
            onClick={() => setShowMoreAgents((open) => !open)}
            className="flex cursor-pointer items-center gap-2 rounded-md py-1.5 text-xs font-medium text-muted-foreground hover:text-foreground"
          >
            <ChevronDownIcon className={cn("size-3.5 transition-transform", !expandedMore && "-rotate-90")} />
            {t("settings.moreAgents", { n: uninstalledAgents.length })}
          </button>
          {expandedMore ? (
            <div className="mt-2 flex flex-col gap-3">
              {uninstalledAgents.map((agent) => (
                <AgentCard key={agent.id} agent={agent} settings={settings?.agents[agent.id]} custom={isCustom(agent.id)} onSave={update} readOnly={remoteClient} />
              ))}
            </div>
          ) : null}
        </div>
      ) : null}
      <CustomAgents
        settings={settings}
        readOnly={remoteClient}
        reservedIds={[...new Set([...BUILTIN_AGENT_IDS, ...listed.filter((agent) => !isCustom(agent.id)).map((agent) => agent.id)])]}
        onSave={async (patch) => {
          const next = await update(patch);
          if (next) {
            const refreshed = await client().request("agents.refresh", {}).catch(() => null);
            if (refreshed) useStore.setState({ agents: refreshed.agents });
          }
          return next;
        }}
      />
    </section>
  );
}

function AgentCard({
  agent,
  settings,
  custom,
  onSave,
  readOnly,
}: {
  agent: AgentInfo;
  settings: AgentSettings | undefined;
  custom: boolean;
  onSave: (patch: unknown) => Promise<Settings | null>;
  readOnly: boolean;
}) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const current: AgentSettings = settings ?? { enabled: true, binaryPath: null, extraArgs: [], env: {} };
  const [binary, setBinary] = useState(current.binaryPath ?? "");
  const [args, setArgs] = useState(current.extraArgs.join("\n"));
  const [env, setEnv] = useState(Object.entries(current.env).map(([k, v]) => `${k}=${v}`).join("\n"));

  useEffect(() => {
    setBinary(current.binaryPath ?? "");
    setArgs(current.extraArgs.join("\n"));
    setEnv(Object.entries(current.env).map(([k, v]) => `${k}=${v}`).join("\n"));
  }, [settings]);

  const provider = agent.auth.provider;
  const authBadge = provider && agent.auth.state !== "required" ? (
    <Badge variant="info">{t("settings.via", { provider })}</Badge>
  ) : agent.auth.state === "ok" ? (
    <Badge variant="success">{agent.auth.account ?? t("settings.authOk")}</Badge>
  ) : agent.auth.state === "required" ? (
      <Badge variant="warning">
        <KeyRoundIcon />
        {t("settings.authRequired")}
      </Badge>
    ) : null;

  const save = () => {
    const envMap: Record<string, string> = {};
    for (const line of env.split("\n")) {
      const index = line.indexOf("=");
      if (index > 0) envMap[line.slice(0, index).trim()] = line.slice(index + 1);
    }
    void onSave({
      agents: {
        [agent.id]: {
          enabled: current.enabled,
          binaryPath: binary.trim() || null,
          extraArgs: args.split("\n").map((a) => a.trim()).filter(Boolean),
          env: envMap,
        },
      },
    });
  };

  return (
    <Group>
      <div className="flex items-center gap-3 px-4 py-3">
        <AgentIcon agentId={agent.id} className="size-6" />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-medium">{agent.name}</span>
            {agent.version ? <span className="text-xs tabular-nums text-muted-foreground" title={agent.version}>{shortVersion(agent.version)}</span> : null}
            {custom ? <Badge variant="outline">{t("settings.customAgent")}</Badge> : null}
            {agent.installed ? authBadge : <Badge variant="error">{t("settings.notInstalled")}</Badge>}
          </div>
          {provider ? (
            <div className="mt-0.5 truncate text-xs text-muted-foreground">
              {t("settings.routing", { provider })}
              {agent.auth.endpoint ? <span className="font-mono text-[11px]"> · {agent.auth.endpoint}</span> : null}
              {agent.auth.account ? <span className="text-muted-foreground/70"> · {t("settings.localSignIn", { account: agent.auth.account })}</span> : null}
            </div>
          ) : null}
          <div className="mt-0.5 truncate font-mono text-[11px] text-muted-foreground" title={agent.path ?? undefined}>
            {displayPath(agent.path) || "—"}
          </div>
          {agent.error ? <div className="mt-0.5 text-xs text-destructive-foreground">{agent.error}</div> : null}
        </div>
        <Switch checked={current.enabled} disabled={readOnly} aria-label={t("settings.enabled")} onCheckedChange={(enabled) => void onSave({ agents: { [agent.id]: { ...current, enabled } } })} />
      </div>
      {agent.installed && agent.rateLimits && agent.rateLimits.length > 0 ? <AgentRateLimits agent={agent} /> : null}
      {agent.installed && agent.auth.state !== "ok" && agent.auth.methods.length > 0 ? (
        <div className={cn("flex flex-col gap-2 px-4 py-2.5", agent.auth.state === "required" && "bg-warning-surface/50")}>
          <span className="text-xs text-muted-foreground">{t(agent.auth.state === "required" ? "composer.signInHint" : "settings.signInOptional", { agent: agent.name })}</span>
          <SignInMethods agent={agent} size="xs" />
        </div>
      ) : null}
      <button
        type="button"
        disabled={readOnly}
        onClick={() => setOpen(!open)}
        className="flex w-full items-center gap-2 px-4 py-2 text-left disabled:cursor-default disabled:opacity-60 text-xs font-medium text-muted-foreground hover:bg-accent/50 hover:text-foreground"
      >
        <ChevronDownIcon className={cn("size-3.5 transition-transform", !open && "-rotate-90")} />
        {t("settings.binaryOverride")} · {t("settings.extraArgs")} · {t("settings.env")}
      </button>
      {open ? (
        <div className="flex flex-col gap-3 px-4 py-3">
          <div className="flex flex-col gap-1.5">
            <Label>{t("settings.binaryOverride")}</Label>
            <Input value={binary} placeholder={agent.path ?? ""} onChange={(e) => setBinary(e.target.value)} className="font-mono text-xs sm:text-xs" />
            <span className="text-xs text-muted-foreground">{t("settings.binaryOverrideHint")}</span>
          </div>
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="flex flex-col gap-1.5">
              <Label>{t("settings.extraArgs")}</Label>
              <Textarea rows={3} value={args} onChange={(e) => setArgs(e.target.value)} />
              <span className="text-xs text-muted-foreground">{t("settings.extraArgsHint")}</span>
            </div>
            <div className="flex flex-col gap-1.5">
              <Label>{t("settings.env")}</Label>
              <Textarea rows={3} value={env} onChange={(e) => setEnv(e.target.value)} />
              <span className="text-xs text-muted-foreground">{t("settings.envHint")}</span>
            </div>
          </div>
          <div className="flex justify-end">
            <Button size="sm" onClick={save}>
              {t("common.save")}
            </Button>
          </div>
        </div>
      ) : null}
    </Group>
  );
}

function AgentRateLimits({ agent }: { agent: AgentInfo }) {
  const t = useT();
  const display = usePrefs((s) => s.usageDisplay);
  const now = useNow(30_000);
  const limits = agent.rateLimits ?? [];
  const age = snapshotAge(agent.rateLimitsUpdatedAt ?? null, now, t);
  const provider = agent.auth.provider;
  return (
    <div className="flex flex-col gap-1.5 px-4 py-2.5">
      {limits.map((limit) => {
        const percent = shownPercent(limit, display);
        const reset = resetLabel(limit, now, t);
        return (
          <div key={limit.label} className="flex items-center gap-3 text-xs tabular-nums">
            <span className="w-16 shrink-0 truncate text-muted-foreground" title={limit.label}>
              {shortLimitLabel(limit.label)}
            </span>
            <div className="h-1 min-w-0 flex-1 overflow-hidden rounded-full bg-muted">
              <div className={cn("h-full rounded-full", limitBarTone(limit))} style={{ width: `${percent}%` }} />
            </div>
            <span className={cn("w-20 shrink-0 text-right", limitTone(limit))}>
              {t(display === "used" ? "usage.used" : "usage.remaining", { n: percent })}
            </span>
            <span className="w-28 shrink-0 truncate text-right text-muted-foreground/80">{reset ?? ""}</span>
          </div>
        );
      })}
      {age || provider ? (
        <div className="text-[11px] text-muted-foreground/80">
          {[age, provider ? t("usage.viaLocal", { provider }) : null].filter(Boolean).join(" · ")}
        </div>
      ) : null}
    </div>
  );
}

function formatMb(bytes: number) {
  return `${Math.max(1, Math.round(bytes / (1024 * 1024)))} MB`;
}

function VoiceSection() {
  const t = useT();
  const voice = useStore((s) => s.voice);
  const progress = useStore((s) => s.voiceProgress);
  const { settings, update } = useSettings();
  const remoteClient = useStore((s) => s.app?.remoteClient ?? false);
  const [draftUrl, setDraftUrl] = useState<string | null>(null);
  const [draftModel, setDraftModel] = useState<string | null>(null);
  const [draftGlossary, setDraftGlossary] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [pendingModel, setPendingModel] = useState<string | null>(null);
  const endpointUrl = draftUrl ?? settings?.voice.endpointUrl ?? "";
  const endpointModel = draftModel ?? settings?.voice.endpointModel ?? "";
  const glossary = draftGlossary ?? Object.entries(settings?.voice.replacements ?? {})
    .map(([from, to]) => `${from} = ${to}`).join("\n");
  const endpointDisabled = remoteClient || !settings || saving;

  useEffect(() => {
    void actions.refreshVoice();
  }, []);

  const installing = !!voice?.installing || (!!progress && !progress.done);
  const percent = progress && progress.totalBytes > 0 ? Math.min(100, Math.round((progress.downloadedBytes / progress.totalBytes) * 100)) : 0;

  const saveVoice = async () => {
    const replacements: Record<string, string> = {};
    for (const line of glossary.split("\n")) {
      const idx = line.indexOf("=");
      if (idx > 0) {
        const from = line.slice(0, idx).trim();
        const to = line.slice(idx + 1).trim();
        if (from) replacements[from] = to;
      }
    }
    setSaving(true);
    const next = await update({
      voice: {
        endpointUrl: endpointUrl.trim() || null,
        endpointModel: endpointModel.trim() || null,
        replacements,
      },
    });
    if (next) {
      setDraftUrl(null);
      setDraftModel(null);
      setDraftGlossary(null);
      await actions.refreshVoice();
    }
    setSaving(false);
  };

  const models = voice?.models ?? [];
  const busy = remoteClient || installing;

  return (
    <section className="flex flex-col gap-6">
      <div>
        <SectionTitle title={t("settings.voice")} action={voice?.mode === "endpoint" ? <Badge variant="info">{t("voice.endpointActive")}</Badge> : undefined} />
        {remoteClient ? <p className="mb-3 text-xs text-muted-foreground">{t("settings.desktopOnly")}</p> : null}
        <h3 className="mb-2 text-sm font-medium text-muted-foreground">{t("voice.models")}</h3>
        <Group>
          {models.length === 0 ? (
            <Row label={<span className="text-muted-foreground">{voice ? voice.modelName : t("common.loading")}</span>} />
          ) : null}
          {models.map((model) => {
            const pending = installing && (pendingModel ?? voice?.activeModelId) === model.id;
            return (
              <Row
                key={model.id}
                label={
                  <span className="flex flex-wrap items-center gap-2">
                    <span>{voiceModelLabel(t, model)}</span>
                    {model.installed && model.active ? (
                      <Badge variant="success">{t("voice.active")}</Badge>
                    ) : model.installed ? (
                      <Badge variant="secondary">{t("voice.installed")}</Badge>
                    ) : (
                      <Badge variant="outline">{t("voice.notInstalled")}</Badge>
                    )}
                  </span>
                }
                hint={
                  model.installed
                    ? `${voiceModelDescription(t, model)} · ${t("voice.diskUsage", { size: formatMb(model.installedBytes) })}`
                    : voiceModelDescription(t, model)
                }
              >
                {pending ? (
                  <Button size="sm" disabled>
                    <Spinner className="size-3.5" />
                    {percent}%
                  </Button>
                ) : model.installed ? (
                  <>
                    {model.active ? null : (
                      <Button size="sm" variant="outline" disabled={busy} onClick={() => void actions.useVoiceModel(model.id)}>
                        {t("voice.use")}
                      </Button>
                    )}
                    <Button size="sm" variant="destructive-outline" disabled={busy} onClick={() => void actions.uninstallVoice(model.id)}>
                      <Trash2Icon />
                      {t("voice.uninstall")}
                    </Button>
                  </>
                ) : (
                  <Button
                    size="sm"
                    variant={model.active ? "default" : "outline"}
                    disabled={busy || voice?.supported === false}
                    onClick={() => {
                      setPendingModel(model.id);
                      void actions.installVoice(model.id).finally(() => setPendingModel(null));
                    }}
                  >
                    <DownloadIcon />
                    {t("voice.download")}
                  </Button>
                )}
              </Row>
            );
          })}
          {installing ? (
            <div className="flex flex-col gap-1.5 px-4 py-3">
              <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
                <div className="h-full bg-primary transition-[width]" style={{ width: `${percent}%` }} />
              </div>
              <div className="flex items-center justify-between text-xs tabular-nums text-muted-foreground">
                <span>{t(progress?.stage.startsWith("extracting") ? "voice.extracting" : "voice.downloading")}</span>
                <span>
                  {progress?.totalBytes ? `${formatMb(progress.downloadedBytes)} / ${formatMb(progress.totalBytes)}` : `${percent}%`}
                </span>
              </div>
            </div>
          ) : null}
          {voice?.error ? <div className="px-4 py-2.5 text-xs text-destructive-foreground">{voice.error}</div> : null}
        </Group>
      </div>
      <div>
        <h3 className="mb-2 text-sm font-medium text-muted-foreground">{t("voice.endpointSection")}</h3>
        <Group>
          <div className="flex flex-col gap-3 px-4 py-3">
            <div className="flex flex-col gap-1.5">
              <Label>{t("voice.endpointUrl")}</Label>
              <Input
                value={endpointUrl}
                disabled={endpointDisabled}
                placeholder="http://127.0.0.1:8080/v1/audio/transcriptions"
                onChange={(event) => setDraftUrl(event.target.value)}
                className="font-mono text-xs sm:text-xs"
              />
              <span className="text-xs text-muted-foreground">{t("voice.endpointUrlHint")}</span>
            </div>
            <div className="flex flex-col gap-1.5">
              <Label>{t("voice.endpointModel")}</Label>
              <Input
                value={endpointModel}
                disabled={endpointDisabled}
                placeholder="whisper-1"
                onChange={(event) => setDraftModel(event.target.value)}
                className="font-mono text-xs sm:text-xs"
              />
            </div>
            <div className="flex flex-col gap-1.5">
              <Label>{t("voice.glossary")}</Label>
              <Textarea
                rows={4}
                value={glossary}
                disabled={endpointDisabled}
                placeholder={"claude code = Claude Code\n维尔 = Verre"}
                onChange={(event) => setDraftGlossary(event.target.value)}
                className="font-mono text-xs sm:text-xs"
              />
              <span className="text-xs text-muted-foreground">{t("voice.glossaryHint")}</span>
            </div>
            <div className="flex justify-end">
              <Button size="sm" disabled={endpointDisabled} onClick={() => void saveVoice()}>
                {t("common.save")}
              </Button>
            </div>
          </div>
        </Group>
      </div>
    </section>
  );
}

function CloseToTraySetting() {
  const t = useT();
  const { settings, update } = useSettings();
  const [saving, setSaving] = useState(false);
  const label = t("settings.keepRunning");
  return (
    <Row label={label} hint={t("settings.keepRunningHint")}>
      <Switch
        aria-label={label}
        checked={settings?.closeToTray ?? false}
        disabled={!settings || saving}
        onCheckedChange={(closeToTray) => {
          setSaving(true);
          void update({ closeToTray }).finally(() => setSaving(false));
        }}
      />
    </Row>
  );
}

function RemoteSection() {
  const t = useT();
  const { embeddedCore } = useDesktopChrome();
  const remote = useStore((s) => s.remote);
  const remoteClient = useStore((s) => s.app?.remoteClient ?? false);
  const [pairing, setPairing] = useState<Pairing | null>(null);
  const [externalUrl, setExternalUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const requestedPairing = useRef(false);
  const now = useNow(1000);

  const refresh = () => {
    setError(null);
    setBusy(true);
    client()
      .request("remote.status", {})
      .then((status) => useStore.setState({ remote: status }))
      .catch((failure) => setError(errorMessage(failure)))
      .finally(() => setBusy(false));
  };
  useEffect(refresh, []);

  const set = (params: RemoteSetParams) => {
    setError(null);
    setBusy(true);
    client()
      .request("remote.set", params)
      .then((status) => {
        useStore.setState({ remote: status });
        if (!status.enabled) setPairing(null);
      })
      .catch((failure) => setError(errorMessage(failure)))
      .finally(() => setBusy(false));
  };

  const createPairing = () => {
    setError(null);
    setBusy(true);
    client()
      .request("remote.createPairing", {})
      .then((result) => {
        useStore.setState((state) => ({
          remote: state.remote && { ...state.remote, pairingExpiresAt: result.expiresAt },
        }));
        setPairing(result);
      })
      .catch((failure) => setError(errorMessage(failure)))
      .finally(() => setBusy(false));
  };

  const address = remote?.externalUrl ?? remote?.tailscale.serveUrl;
  useEffect(() => {
    if (address && !remoteClient && !requestedPairing.current && new URLSearchParams(location.search).get("pairing") === "1") {
      requestedPairing.current = true;
      createPairing();
    }
  }, [address, remoteClient]);

  if (!remote) return error ? (
    <div className="flex items-center justify-between gap-3">
      <p role="alert" className="text-sm text-destructive-foreground">{error}</p>
      <Button size="sm" variant="outline" onClick={refresh}>{t("common.retry")}</Button>
    </div>
  ) : <Spinner className="mx-auto my-10 block text-muted-foreground" />;
  const tailscale = remote.tailscale;
  const visiblePairing = pairing && pairing.expiresAt === remote.pairingExpiresAt ? pairing : null;
  return (
    <section className="flex flex-col gap-6">
      {!remoteClient ? <div>
        <SectionTitle title={t("settings.remote")} />
        {error ? <p role="alert" className="mb-3 rounded-lg border border-destructive/30 bg-error-surface px-3 py-2 text-sm text-destructive-foreground">{error}</p> : null}
        <Group>
          {embeddedCore ? <CloseToTraySetting /> : null}
          {remote.enabled ? <Row
            label={t("settings.remoteActive")}
            hint={address ? <span className="break-all font-mono">{address}</span> : t("settings.tailscaleStopped")}
          >
            <Button size="sm" variant="outline" disabled={busy} onClick={() => set({ enabled: false, tailscaleServe: false })}>
              {t("settings.remoteStop")}
            </Button>
          </Row> : null}
          {!address ? <Row
            label={t("settings.tailscale")}
            hint={t("settings.remoteEnableHint")}
          >
            <Badge variant={!tailscale.installed ? "error" : tailscale.running ? "success" : "warning"}>
              {!tailscale.installed ? t("settings.tailscaleMissing") : tailscale.running ? t("settings.tailscaleRunning") : t("settings.tailscaleStopped")}
            </Badge>
            {tailscale.running ? <Button size="sm" variant="outline" disabled={busy} onClick={() => set({ enabled: true, tailscaleServe: true })}>
              {t("settings.tailscaleServe")}
            </Button> : <Button size="sm" variant="outline" disabled={busy} onClick={refresh}>
              {t("settings.remoteRefresh")}
            </Button>}
          </Row> : null}
          {!address ? <form className="grid gap-3 px-4 py-3" onSubmit={(event) => {
            event.preventDefault();
            if (!busy && externalUrl.trim()) set({ enabled: true, tailscaleServe: false, externalUrl: externalUrl.trim() });
          }}>
            <div>
              <Label htmlFor="remote-proxy-url">{t("settings.remoteProxy")}</Label>
              <p id="remote-proxy-target" className="mt-0.5 text-xs text-muted-foreground">
                {t("settings.remoteProxyTarget", { target: `http://127.0.0.1:${remote.port}` })}
              </p>
            </div>
            <div className="flex items-center gap-2">
              <Input
                id="remote-proxy-url"
                type="url"
                aria-label={t("settings.remoteProxyUrl")}
                aria-describedby="remote-proxy-target"
                placeholder="https://verre.example.com"
                autoCapitalize="none"
                spellCheck={false}
                disabled={busy}
                value={externalUrl}
                onChange={(event) => setExternalUrl(event.target.value)}
              />
              <Button size="sm" variant="outline" type="submit" disabled={busy || !externalUrl.trim()}>
                {t("settings.remoteStart")}
              </Button>
            </div>
          </form> : null}
        </Group>
      </div> : null}

      {!remoteClient && address ? <div>
        <SectionTitle
          title={t("settings.pairDevice")}
          action={
            <Button size="sm" variant="outline" disabled={busy} onClick={createPairing}>
              <SmartphoneIcon />
              {visiblePairing ? t("settings.newCode") : t("settings.pairDevice")}
            </Button>
          }
        />
        {visiblePairing ? (
          <Group>
            <div className="flex flex-col items-center gap-5 px-4 py-5 sm:flex-row sm:items-start">
              {visiblePairing.expiresAt > now ? <>
                {visiblePairing.url ? <QrCodeSvg value={visiblePairing.url} size={148} margin={4} /> : null}
                <div className="flex min-w-0 flex-col items-center gap-2 sm:items-start">
                  <div className="font-mono text-3xl font-semibold tracking-[0.2em] tabular-nums">{visiblePairing.code}</div>
                  <p className="text-center text-xs text-muted-foreground sm:text-left">
                    {t("settings.pairHint", { time: countdown(visiblePairing.expiresAt - now) })}
                  </p>
                  {visiblePairing.url ? <Button size="sm" variant="outline" onClick={() => {
                    void copyText(visiblePairing.url!).then((ok) => ok && toast.success(t("common.copied")));
                  }}>{t("settings.remoteCopyLink")}</Button> : null}
                </div>
              </> : <p className="text-sm text-muted-foreground">{t("settings.pairExpired")}</p>}
            </div>
          </Group>
        ) : null}
      </div> : null}

      <div>
        <SectionTitle title={t("settings.devices")} />
        <Group>
          {remote.devices.length === 0 ? <Row label={<span className="text-muted-foreground">{t("settings.noDevices")}</span>} /> : null}
          {remote.devices.map((device) => (
            <Row
              key={device.id}
              label={
                <span className="flex items-center gap-2">
                  <SmartphoneIcon className="size-4 text-muted-foreground" />
                  {device.name}
                  {device.current ? <Badge variant="info">{t("settings.thisDevice")}</Badge> : null}
                </span>
              }
              hint={device.lastSeenAt ? t("settings.lastSeen", { time: relativeTime(device.lastSeenAt, t) }) : t("settings.neverSeen")}
            >
              <Button
                size="xs"
                variant="destructive-outline"
                onClick={() =>
                  void client()
                    .request("remote.revokeDevice", { deviceId: device.id })
                    .then((status) => useStore.setState({ remote: status }))
                    .catch((error) => toast.error(errorMessage(error)))
                }
              >
                {t("settings.revoke")}
              </Button>
            </Row>
          ))}
        </Group>
      </div>
    </section>
  );
}

const PRESET_DAYS = [7, 14, 30, 0];

function ActivityWindow() {
  const t = useT();
  const days = usePrefs((s) => s.sidebarActivityDays);
  const [custom, setCustom] = useState(!PRESET_DAYS.includes(days));
  const [draft, setDraft] = useState(String(days));
  useEffect(() => setDraft(String(days)), [days]);
  const commit = () => {
    const value = Math.round(Number(draft));
    if (Number.isFinite(value) && value > 0 && value !== days) setSidebarActivityDays(Math.min(value, 3650));
    else setDraft(String(days));
  };
  return (
    <div className="flex flex-wrap items-center justify-end gap-2">
      {custom ? (
        <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
          <Input
            type="number"
            min={1}
            max={3650}
            value={draft}
            aria-label={t("settings.activityCustomDays")}
            onChange={(event) => setDraft(event.target.value)}
            onBlur={commit}
            onKeyDown={(event) => event.key === "Enter" && commit()}
            className="h-7 w-16 text-right tabular-nums sm:h-7"
          />
          {t("settings.activityDaysUnit")}
        </span>
      ) : null}
      <Segmented<string>
        value={custom ? "custom" : String(days)}
        onChange={(value) => {
          if (value === "custom") {
            setCustom(true);
            return;
          }
          setCustom(false);
          setSidebarActivityDays(Number(value));
        }}
        options={[
          { value: "7", label: t("settings.activityDays", { n: 7 }) },
          { value: "14", label: t("settings.activityDays", { n: 14 }) },
          { value: "30", label: t("settings.activityDays", { n: 30 }) },
          { value: "0", label: t("settings.activityAll") },
          { value: "custom", label: t("settings.activityCustom") },
        ]}
      />
    </div>
  );
}

const EDITORS = ["auto", "cursor", "code", "zed", "system"] as const;

function EditorSelect({ value, disabled, onChange }: { value: string | null; disabled: boolean; onChange: (value: string | null) => void }) {
  const t = useT();
  const current = value ?? "auto";
  const known = (EDITORS as readonly string[]).includes(current);
  const label = (id: string) => t(`settings.editor.${id as (typeof EDITORS)[number]}`);
  return (
    <Menu>
      <MenuTrigger disabled={disabled} render={<Button size="sm" variant="outline" className="min-w-32 justify-between" />}>
        <span className="truncate">{known ? label(current) : current}</span>
        <ChevronDownIcon className="size-3.5 opacity-70" />
      </MenuTrigger>
      <MenuPopup align="end" className="w-44">
        <MenuRadioGroup value={current} onValueChange={(next: string) => onChange(next === "auto" ? null : next)}>
          {EDITORS.map((id) => (
            <MenuRadioItem key={id} value={id}>
              {label(id)}
            </MenuRadioItem>
          ))}
        </MenuRadioGroup>
      </MenuPopup>
    </Menu>
  );
}

function LanguageSelect({ value, labelledBy, onChange }: { value: LanguagePref; labelledBy: string; onChange: (value: LanguagePref) => void }) {
  const t = useT();
  const valueId = useId();
  const currentLabel = value === "system" ? t("settings.languageSystem") : LOCALE_LABELS[value];
  return (
    <Menu>
      <MenuTrigger aria-labelledby={`${labelledBy} ${valueId}`} render={<Button size="sm" variant="outline" className="min-w-36 justify-between" />}>
        <span id={valueId} lang={value === "system" ? undefined : htmlLangTag(value)} className="truncate">
          {currentLabel}
        </span>
        <ChevronDownIcon className="size-3.5 opacity-70" />
      </MenuTrigger>
      <MenuPopup align="end" className="w-44">
        <MenuRadioGroup value={value} onValueChange={(next: string) => onChange(next as LanguagePref)}>
          <MenuRadioItem value="system">{t("settings.languageSystem")}</MenuRadioItem>
          {LOCALES.map((code) => (
            <MenuRadioItem key={code} value={code} lang={htmlLangTag(code)}>
              {LOCALE_LABELS[code]}
            </MenuRadioItem>
          ))}
        </MenuRadioGroup>
      </MenuPopup>
    </Menu>
  );
}

function GeneralSection() {
  const t = useT();
  const languageLabelId = useId();
  const language = usePrefs((s) => s.language);
  const { settings, update } = useSettings();
  const remoteClient = useStore((s) => s.app?.remoteClient ?? false);
  const [idle, setIdle] = useState<string>("");
  useEffect(() => {
    if (settings) setIdle(String(settings.idleReleaseMinutes));
  }, [settings]);
  return (
    <section>
      <SectionTitle title={t("settings.general")} />
      <Group>
        <Row label={t("settings.language")} labelId={languageLabelId}>
          <LanguageSelect value={language} labelledBy={languageLabelId} onChange={setLanguage} />
        </Row>
      </Group>
      {remoteClient ? <p className="mt-4 mb-2 text-xs text-muted-foreground">{t("settings.desktopOnly")}</p> : null}
      <Group className={remoteClient ? undefined : "mt-4"}>
        <Row label={t("settings.externalEditor")} hint={t("settings.externalEditorHint")}>
          <EditorSelect value={settings?.externalEditor ?? null} disabled={remoteClient || !settings} onChange={(externalEditor) => void update({ externalEditor })} />
        </Row>
        <Row label={t("settings.idleRelease")} hint={t("settings.idleReleaseHint")}>
          <Input
            type="number"
            min={0}
            disabled={remoteClient || !settings}
            value={idle}
            aria-label={t("settings.idleRelease")}
            onChange={(event) => setIdle(event.target.value)}
            onBlur={() => {
              const minutes = Math.max(0, Math.round(Number(idle) || 0));
              if (settings && minutes !== settings.idleReleaseMinutes) void update({ idleReleaseMinutes: minutes });
              else if (settings) setIdle(String(settings.idleReleaseMinutes));
            }}
            className="w-20 text-right tabular-nums"
          />
        </Row>
      </Group>
    </section>
  );
}

function SubTitle({ children, first }: { children: ReactNode; first?: boolean }) {
  return <h3 className={cn("mb-2 text-sm font-medium text-muted-foreground", !first && "mt-6")}>{children}</h3>;
}

function ThreadsSection() {
  const t = useT();
  const showLimits = usePrefs((s) => s.composerShowLimits);
  const usageDisplay = usePrefs((s) => s.usageDisplay);
  const showContext = usePrefs((s) => s.composerShowContext);
  const sidebarOrder = usePrefs((s) => s.sidebarOrder);
  const hideDisabled = usePrefs((s) => s.hideDisabledAgentHistory);
  const bypassMemoryScope = usePrefs((s) => s.bypassMemoryScope);
  const limitsMode: "off" | UsageDisplay = showLimits ? usageDisplay : "off";
  return (
    <section>
      <SectionTitle title={t("settings.threads")} />
      <SubTitle first>{t("settings.usageLimits")}</SubTitle>
      <Group>
        <Row label={t("settings.composerShowLimits")} hint={t("settings.composerShowLimitsHint")}>
          <Segmented<"off" | UsageDisplay>
            label={t("settings.composerShowLimits")}
            value={limitsMode}
            onChange={(value) => {
              setComposerShowLimits(value !== "off");
              if (value !== "off") setUsageDisplay(value);
            }}
            options={[
              { value: "off", label: t("settings.usageDisplay.off") },
              { value: "remaining", label: t("settings.usageDisplay.remaining") },
              { value: "used", label: t("settings.usageDisplay.used") },
            ]}
          />
        </Row>
        <Row label={t("settings.composerShowContext")}>
          <Switch checked={showContext} aria-label={t("settings.composerShowContext")} onCheckedChange={setComposerShowContext} />
        </Row>
      </Group>
      <SubTitle>{t("settings.sidebarHistory")}</SubTitle>
      <Group>
        <Row label={t("settings.sidebarOrder")}>
          <Segmented<SidebarOrder>
            value={sidebarOrder}
            onChange={setSidebarOrder}
            options={[
              { value: "projects", label: t("settings.sidebarOrder.projects") },
              { value: "quickChats", label: t("settings.sidebarOrder.quickChats") },
            ]}
          />
        </Row>
        <Row label={t("settings.activityWindow")} hint={t("settings.activityWindowHint")}>
          <ActivityWindow />
        </Row>
        <Row label={t("settings.hideDisabledHistory")} hint={t("settings.hideDisabledHistoryHint")}>
          <Switch checked={hideDisabled} aria-label={t("settings.hideDisabledHistory")} onCheckedChange={setHideDisabledAgentHistory} />
        </Row>
      </Group>
      <SubTitle>{t("settings.permissions")}</SubTitle>
      <Group>
        <Row label={t("settings.bypassMemory")} hint={t("settings.bypassMemoryHint")}>
          <Segmented<BypassMemoryScope>
            value={bypassMemoryScope}
            onChange={setBypassMemoryScope}
            options={[
              { value: "project", label: t("settings.bypassMemory.project") },
              { value: "global", label: t("settings.bypassMemory.global") },
            ]}
          />
        </Row>
      </Group>
    </section>
  );
}

function AppearanceSection() {
  const t = useT();
  const theme = usePrefs((s) => s.theme);
  const fontStyle = usePrefs((s) => s.fontStyle);
  const fontScale = usePrefs((s) => s.fontScale);
  const chatWidth = usePrefs((s) => s.chatWidth);
  const paperGrain = usePrefs((s) => s.paperGrain);
  return (
    <section>
      <SectionTitle title={t("settings.appearance")} />
      <Group>
        <Row label={t("settings.theme")}>
          <Segmented<ThemePref>
            value={theme}
            onChange={setTheme}
            options={[
              { value: "light", label: t("settings.themeLight"), icon: SunIcon },
              { value: "dark", label: t("settings.themeDark"), icon: MoonIcon },
              { value: "system", label: t("settings.themeSystem"), icon: MonitorIcon },
            ]}
          />
        </Row>
      </Group>
      <ThemeGallery />
      <SubTitle>{t("settings.readingLayout")}</SubTitle>
      <Group>
        <Row label={t("settings.fontStyle")} hint={t("settings.fontStyleHint")}>
          <Segmented<FontStyle>
            value={fontStyle}
            onChange={setFontStyle}
            options={[
              { value: "sans", label: t("settings.font.sans") },
              { value: "serif", label: t("settings.font.serif") },
              { value: "mono", label: t("settings.font.mono") },
            ]}
          />
        </Row>
        <Row label={t("settings.fontScale")}>
          <Segmented<FontScale>
            value={fontScale}
            onChange={setFontScale}
            options={[
              { value: "compact", label: t("settings.fontScale.compact") },
              { value: "default", label: t("settings.fontScale.default") },
              { value: "large", label: t("settings.fontScale.large") },
            ]}
          />
        </Row>
        <Row label={t("settings.chatWidth")}>
          <Segmented<ChatWidth>
            value={chatWidth}
            onChange={setChatWidth}
            options={[
              { value: "standard", label: t("settings.chatWidth.standard") },
              { value: "wide", label: t("settings.chatWidth.wide") },
              { value: "full", label: t("settings.chatWidth.full") },
            ]}
          />
        </Row>
        <Row label={t("settings.paperGrain")} hint={t("settings.paperGrainHint")}>
          <Switch checked={paperGrain} aria-label={t("settings.paperGrain")} onCheckedChange={setPaperGrain} />
        </Row>
      </Group>
    </section>
  );
}

function NotificationsSection() {
  const t = useT();
  const { settings, update } = useSettings();
  const remoteClient = useStore((s) => s.app?.remoteClient ?? false);
  const notif = resolvedNotificationSettings(settings);
  const disabled = remoteClient || !settings;
  const alertsOff = notif.mode === "off";
  const modes: NotificationMode[] = ["off", "notifications", "sound", "notifications_and_sound"];
  const modeLabel = (mode: NotificationMode) => t(`settings.notificationsMode.${mode}`);
  const patchNotif = (patch: Partial<NotificationSettings>) => void update({ notifications: patch });
  const testAlert = () => {
    unlockNotificationAudio();
    if (alertsOff) {
      toast.info(t("settings.notificationsTestOff"));
      return;
    }
    if (hasSound(notif.mode)) void playNotificationChime("complete");
    if (hasPopup(notif.mode)) {
      if (!remoteClient) {
        void client()
          .request("app.notify", {
            title: t("app.name"),
            body: t("settings.notificationsTestBody"),
          })
          .catch((error) => toast.error(errorMessage(error)));
      } else {
        toast.info(t("settings.desktopOnly"));
      }
    } else if (notif.mode === "sound") {
      toast.success(t("settings.notificationsTestSound"));
    }
  };
  return (
    <section>
      <SectionTitle
        title={t("settings.notifications")}
        action={
          <Button size="sm" variant="outline" onClick={testAlert}>
            <BellIcon />
            {t("settings.notificationsTest")}
          </Button>
        }
      />
      {remoteClient ? <p className="mb-3 text-xs text-muted-foreground">{t("settings.desktopOnly")}</p> : null}
      <Group>
        <Row label={t("settings.notificationsMode")} hint={t("settings.notificationsHint")}>
          <Menu>
            <MenuTrigger disabled={disabled} render={<Button size="sm" variant="outline" className="min-w-40 justify-between" />}>
              <span className="truncate">{modeLabel(notif.mode)}</span>
              <ChevronDownIcon className="size-3.5 opacity-70" />
            </MenuTrigger>
            <MenuPopup align="end" className="w-48">
              <MenuRadioGroup
                value={notif.mode}
                onValueChange={(next) => {
                  const mode = next as NotificationMode;
                  if (hasSound(mode)) unlockNotificationAudio();
                  patchNotif({ mode });
                }}
              >
                {modes.map((mode) => (
                  <MenuRadioItem key={mode} value={mode}>
                    {modeLabel(mode)}
                  </MenuRadioItem>
                ))}
              </MenuRadioGroup>
            </MenuPopup>
          </Menu>
        </Row>
        {!alertsOff ? (
          <Row label={t("settings.notificationsBackgroundOnly")} hint={t("settings.notificationsBackgroundOnlyHint")}>
            <Switch
              checked={notif.backgroundOnly}
              disabled={disabled}
              aria-label={t("settings.notificationsBackgroundOnly")}
              onCheckedChange={(backgroundOnly) => patchNotif({ backgroundOnly })}
            />
          </Row>
        ) : null}
      </Group>
      {!alertsOff ? (
        <>
          <SubTitle>{t("settings.notificationsEvents")}</SubTitle>
          <Group>
            <Row label={t("settings.notificationsOnComplete")}>
              <Switch
                checked={notif.onComplete}
                disabled={disabled}
                aria-label={t("settings.notificationsOnComplete")}
                onCheckedChange={(onComplete) => patchNotif({ onComplete })}
              />
            </Row>
            <Row label={t("settings.notificationsOnInput")}>
              <Switch
                checked={notif.onInput}
                disabled={disabled}
                aria-label={t("settings.notificationsOnInput")}
                onCheckedChange={(onInput) => patchNotif({ onInput })}
              />
            </Row>
            <Row label={t("settings.notificationsOnError")}>
              <Switch
                checked={notif.onError}
                disabled={disabled}
                aria-label={t("settings.notificationsOnError")}
                onCheckedChange={(onError) => patchNotif({ onError })}
              />
            </Row>
          </Group>
        </>
      ) : null}
      <SubTitle>{t("settings.notificationsBadgeGroup")}</SubTitle>
      <Group>
        <Row label={t("settings.notificationsBadge")} hint={t("settings.notificationsBadgeHint")}>
          <Switch
            checked={notif.badgeEnabled}
            disabled={disabled}
            aria-label={t("settings.notificationsBadge")}
            onCheckedChange={(badgeEnabled) => patchNotif({ badgeEnabled })}
          />
        </Row>
        {notif.badgeEnabled ? (
          <>
            <Row label={t("settings.notificationsBadgeScope")}>
              <Segmented<BadgeScope>
                value={notif.badgeScope}
                disabled={disabled}
                onChange={(badgeScope) => patchNotif({ badgeScope })}
                options={[
                  { value: "attention", label: t("settings.notificationsBadgeScope.attention") },
                  { value: "needs_input", label: t("settings.notificationsBadgeScope.needs_input") },
                ]}
              />
            </Row>
            <Row label={t("settings.notificationsBadgeClear")}>
              <Segmented<BadgeClearMode>
                value={notif.badgeClear}
                disabled={disabled}
                onChange={(badgeClear) => patchNotif({ badgeClear })}
                options={[
                  { value: "on_thread_open", label: t("settings.notificationsBadgeClear.on_thread_open") },
                  { value: "on_window_focus", label: t("settings.notificationsBadgeClear.on_window_focus") },
                ]}
              />
            </Row>
          </>
        ) : null}
      </Group>
    </section>
  );
}

function AboutSection() {
  const t = useT();
  const app = useStore((s) => s.app);
  const updateCheck = useStore((s) => s.updateCheck);
  const updateChecking = useStore((s) => s.updateChecking);
  const updateInstalling = useStore((s) => s.updateInstalling);
  const updateConfirm = useStore((s) => s.updateConfirm);
  const updateProgress = useStore((s) => s.updateProgress);
  const remoteClient = app?.remoteClient ?? false;

  const progressPercent =
    updateProgress?.totalBytes && updateProgress.totalBytes > 0
      ? Math.min(100, Math.round((updateProgress.downloadedBytes / updateProgress.totalBytes) * 100))
      : 0;

  const statusText = updateProgress?.error
    ? updateProgress.error
    : updateInstalling || (updateProgress && !updateProgress.done)
      ? updateProgress?.stage === "installing"
        ? t("update.installing")
        : t("update.downloading", { percent: progressPercent })
      : updateProgress?.stage === "restarting"
        ? t("update.restarting")
        : updateCheck?.available && updateCheck.latestVersion
          ? t("update.available", { version: updateCheck.latestVersion })
          : updateCheck
            ? t("update.upToDate")
            : t("update.notChecked");

  return (
    <section>
      <SectionTitle title={t("settings.about")} />
      <Group>
        <Row label={t("settings.version")}>
          <span className="font-mono text-xs text-muted-foreground">{app?.version}</span>
        </Row>
        <Row label={t("update.title")} hint={statusText}>
          <div className="flex flex-wrap items-center justify-end gap-2">
            <Button
              size="sm"
              variant="outline"
              disabled={updateChecking || updateInstalling}
              onClick={() => void actions.checkUpdate(false)}
            >
              {updateChecking ? <Spinner className="size-3.5" /> : <RefreshCwIcon className="size-3.5" />}
              {updateChecking ? t("update.checking") : t("update.check")}
            </Button>
            {updateCheck?.available && !remoteClient ? (
              <Button
                size="sm"
                disabled={updateInstalling}
                onClick={() => void actions.installUpdate()}
              >
                {updateInstalling ? <Spinner className="size-3.5" /> : <DownloadIcon className="size-3.5" />}
                {updateCheck.canAutoInstall ? t("update.installRestart") : t("update.downloadPage")}
              </Button>
            ) : null}
          </div>
        </Row>
        {updateProgress && !updateProgress.done && updateProgress.stage === "downloading" ? (
          <div className="px-4 py-2.5">
            <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
              <div className="h-full bg-primary transition-[width]" style={{ width: `${progressPercent}%` }} />
            </div>
          </div>
        ) : null}
        {updateCheck?.available && updateCheck.notes ? (
          <div className="px-4 py-3">
            <pre className="max-h-40 overflow-y-auto whitespace-pre-wrap font-sans text-xs leading-relaxed text-muted-foreground">
              {updateCheck.notes}
            </pre>
          </div>
        ) : null}
        <Row label={t("settings.dataDir")}>
          <span className="max-w-xs truncate font-mono text-xs text-muted-foreground">{app?.dataDir}</span>
        </Row>
        <Row label={t("settings.host")}>
          <span className="font-mono text-xs text-muted-foreground">
            {app?.hostname} · {app?.platform}
          </span>
        </Row>
        <Row label={t("settings.protocol")}>
          <span className="font-mono text-xs text-muted-foreground">v{app?.protocolVersion}</span>
        </Row>
      </Group>
      {updateConfirm ? (
        <Dialog open onOpenChange={(open) => !open && actions.dismissUpdateConfirm()}>
          <DialogPopup className="max-w-md">
            <div className="flex flex-col gap-4 p-5">
              <div className="flex flex-col gap-1.5 pe-8">
                <DialogTitle>{t("update.activeWorkTitle")}</DialogTitle>
                <DialogDescription>{t("update.activeWorkBody")}</DialogDescription>
              </div>
              <div className="flex justify-end gap-2">
                <Button variant="ghost" onClick={() => actions.dismissUpdateConfirm()}>
                  {t("common.cancel")}
                </Button>
                <Button
                  variant="destructive"
                  onClick={() => {
                    actions.dismissUpdateConfirm();
                    void actions.installUpdate(true);
                  }}
                >
                  {t("update.activeWorkConfirm")}
                </Button>
              </div>
            </div>
          </DialogPopup>
        </Dialog>
      ) : null}
    </section>
  );
}
