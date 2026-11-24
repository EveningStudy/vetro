import {
  BotIcon,
  ChevronDownIcon,
  InfoIcon,
  KeyRoundIcon,
  MonitorIcon,
  MoonIcon,
  PaletteIcon,
  RefreshCwIcon,
  SmartphoneIcon,
  SunIcon,
  WifiIcon,
} from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { client } from "~/api/client";
import type { AgentInfo, AgentSettings, Pairing, Settings } from "~/api/types";
import { useT } from "~/i18n";
import { relativeTime, shortVersion } from "~/lib/format";
import { cn } from "~/lib/utils";
import {
  type LanguagePref,
  type SidebarScope,
  type ThemePref,
  setHideDisabledAgentHistory,
  setLanguage,
  setSidebarActivityDays,
  setSidebarScope,
  setTheme,
  usePrefs,
} from "~/state/prefs";
import { type SettingsSection, actions, useStore } from "~/state/store";
import { errorMessage, toast } from "~/state/toast";
import { AgentIcon } from "../AgentIcon";
import { CustomAgents } from "./CustomAgents";
import { SignInMethods } from "../SignIn";
import { QrCodeSvg } from "../QrCode";
import { MobileSidebarButton } from "../Shell";
import { useNow } from "../hooks";
import { Button } from "../ui/button";
import { Menu, MenuPopup, MenuRadioGroup, MenuRadioItem, MenuTrigger } from "../ui/overlay";
import { Badge, Input, Label, Spinner, Switch, Textarea } from "../ui/primitives";

const SECTIONS: { id: SettingsSection; icon: typeof BotIcon }[] = [
  { id: "agents", icon: BotIcon },
  { id: "remote", icon: WifiIcon },
  { id: "appearance", icon: PaletteIcon },
  { id: "about", icon: InfoIcon },
];

export function SettingsView({ section }: { section: SettingsSection }) {
  const t = useT();
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex h-(--workspace-topbar-height) shrink-0 items-center gap-2 px-(--workspace-gutter)">
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
              {t(`settings.${id}`)}
            </button>
          ))}
        </nav>
        <div className="min-h-0 flex-1 overflow-y-auto">
          <div className="mx-auto w-full max-w-2xl px-(--workspace-gutter) pt-2 pb-16 md:pt-0">
            {section === "agents" ? <AgentsSection /> : null}
            {section === "remote" ? <RemoteSection /> : null}
            {section === "appearance" ? <AppearanceSection /> : null}
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
  return <div className={cn("divide-y overflow-hidden rounded-xl border bg-card/70 dark:bg-white/[0.015]", className)}>{children}</div>;
}

function Row({ label, hint, children }: { label: ReactNode; hint?: ReactNode; children?: ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-4 px-4 py-3 sm:gap-6">
      <div className="min-w-0">
        <div className="text-sm text-foreground">{label}</div>
        {hint ? <div className="mt-0.5 text-xs text-muted-foreground">{hint}</div> : null}
      </div>
      {children ? <div className="flex shrink-0 items-center gap-2">{children}</div> : null}
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
  const t = useT();
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
      toast.success(t("settings.saved"));
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
  const remoteClient = useStore((s) => s.app?.remoteClient ?? false);
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
        {agents.map((agent) => (
          <AgentCard key={agent.id} agent={agent} settings={settings?.agents[agent.id]} custom={settings?.customAcpAgents.some((c) => c.id === agent.id) ?? false} onSave={update} readOnly={remoteClient} />
        ))}
      </div>
      <CustomAgents
        settings={settings}
        readOnly={remoteClient}
        reservedIds={agents.filter((agent) => !settings?.customAcpAgents.some((c) => c.id === agent.id)).map((agent) => agent.id)}
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

function RemoteSection() {
  const t = useT();
  const remote = useStore((s) => s.remote);
  const remoteClient = useStore((s) => s.app?.remoteClient ?? false);
  const [pairing, setPairing] = useState<Pairing | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const now = useNow(1000);

  useEffect(() => {
    client()
      .request("remote.status", {})
      .then((status) => useStore.setState({ remote: status }))
      .catch((error) => toast.error(errorMessage(error)));
  }, []);

  const set = (enabled: boolean, tailscaleServe: boolean) => {
    setBusy(true);
    client()
      .request("remote.set", { enabled, tailscaleServe })
      .then((status) => {
        setError(null);
        useStore.setState({ remote: status });
        if (!status.enabled) setPairing(null);
      })
      .catch((failure) => setError(errorMessage(failure)))
      .finally(() => setBusy(false));
  };

  const createPairing = () =>
    client()
      .request("remote.createPairing", {})
      .then((result) => {
        setError(null);
        setPairing(result);
      })
      .catch((failure) => setError(errorMessage(failure)));

  useEffect(() => {
    if (new URLSearchParams(location.search).get("pairing") === "1" && remote?.enabled && !pairing) void createPairing();
  }, [remote?.enabled]);

  if (!remote) return <Spinner className="mx-auto my-10 block text-muted-foreground" />;
  const tailscale = remote.tailscale;
  const serving = !!tailscale.serveUrl;
  return (
    <section className="flex flex-col gap-6">
      <div>
        <SectionTitle title={t("settings.remote")} />
        {remoteClient ? <p className="mb-3 text-xs text-muted-foreground">{t("settings.desktopOnly")}</p> : null}
        {error ? <p className="mb-3 rounded-lg border border-destructive/30 bg-error-surface px-3 py-2 text-sm text-destructive-foreground">{error}</p> : null}
        <Group>
          <Row label={t("settings.remoteEnable")} hint={t("settings.remoteEnableHint")}>
            <Switch checked={remote.enabled} disabled={remoteClient || busy} onCheckedChange={(enabled) => set(enabled, serving)} />
          </Row>
          <Row
            label={t("settings.tailscale")}
            hint={tailscale.dnsName ? <span className="font-mono">{tailscale.dnsName}</span> : undefined}
          >
            <Badge variant={!tailscale.installed ? "error" : tailscale.running ? "success" : "warning"}>
              {!tailscale.installed ? t("settings.tailscaleMissing") : tailscale.running ? t("settings.tailscaleRunning") : t("settings.tailscaleStopped")}
            </Badge>
          </Row>
          <Row label={t("settings.tailscaleServe")} hint={tailscale.serveUrl ? <span className="font-mono">{tailscale.serveUrl}</span> : `127.0.0.1:${remote.port}`}>
            <Switch checked={serving} disabled={remoteClient || busy || !tailscale.running || !remote.enabled} onCheckedChange={(serve) => set(remote.enabled, serve)} />
          </Row>
        </Group>
      </div>

      <div>
        <SectionTitle
          title={t("settings.pairDevice")}
          action={
            <Button size="sm" variant="outline" disabled={remoteClient || !remote.enabled} onClick={() => void createPairing()}>
              <SmartphoneIcon />
              {pairing ? t("settings.newCode") : t("settings.pairDevice")}
            </Button>
          }
        />
        {pairing ? (
          <Group>
            <div className="flex flex-col items-center gap-5 px-4 py-5 sm:flex-row sm:items-start">
              {pairing.url ? <QrCodeSvg value={pairing.url} size={148} /> : null}
              <div className="flex min-w-0 flex-col items-center gap-2 sm:items-start">
                <div className="font-mono text-3xl font-semibold tracking-[0.2em] tabular-nums">{pairing.code}</div>
                {pairing.url ? <div className="max-w-full truncate font-mono text-xs text-muted-foreground">{pairing.url}</div> : null}
                <p className="text-center text-xs text-muted-foreground sm:text-left">
                  {pairing.expiresAt > now
                    ? t("settings.pairHint", { time: countdown(pairing.expiresAt - now) })
                    : t("settings.pairExpired")}
                </p>
              </div>
            </div>
          </Group>
        ) : null}
      </div>

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

function Segmented<T extends string>({ value, options, onChange }: { value: T; options: { value: T; label: string; icon?: typeof SunIcon }[]; onChange: (value: T) => void }) {
  return (
    <div className="flex rounded-lg bg-input/40 p-0.5" role="radiogroup">
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          role="radio"
          aria-checked={value === option.value}
          onClick={() => onChange(option.value)}
          className={cn(
            "flex h-7 items-center gap-1.5 rounded-md px-2.5 text-xs font-medium text-muted-foreground hover:text-foreground",
            value === option.value && "bg-background text-foreground shadow-xs dark:bg-input/72",
          )}
        >
          {option.icon ? <option.icon className="size-3.5" /> : null}
          {option.label}
        </button>
      ))}
    </div>
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

function WorkspaceSettings() {
  const t = useT();
  const { settings, update } = useSettings();
  const remoteClient = useStore((s) => s.app?.remoteClient ?? false);
  return (
    <>
      <h3 className="mt-6 mb-2 text-sm font-medium text-muted-foreground">{t("settings.workspaceNotifications")}</h3>
      {remoteClient ? <p className="mb-2 text-xs text-muted-foreground">{t("settings.desktopOnly")}</p> : null}
      <Group>
        <Row label={t("settings.externalEditor")} hint={t("settings.externalEditorHint")}>
          <EditorSelect value={settings?.externalEditor ?? null} disabled={remoteClient || !settings} onChange={(externalEditor) => void update({ externalEditor })} />
        </Row>
        <Row label={t("settings.notifications")} hint={t("settings.notificationsHint")}>
          <Switch
            checked={settings?.notificationsEnabled ?? true}
            disabled={remoteClient || !settings}
            aria-label={t("settings.notifications")}
            onCheckedChange={(notificationsEnabled) => void update({ notificationsEnabled })}
          />
        </Row>
      </Group>
    </>
  );
}

function AppearanceSection() {
  const t = useT();
  const theme = usePrefs((s) => s.theme);
  const language = usePrefs((s) => s.language);
  const scope = usePrefs((s) => s.sidebarScope);
  const hideDisabled = usePrefs((s) => s.hideDisabledAgentHistory);
  const showInternal = useStore((s) => s.filters.showInternal);
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
        <Row label={t("settings.language")}>
          <Segmented<LanguagePref>
            value={language}
            onChange={setLanguage}
            options={[
              { value: "system", label: t("settings.languageSystem") },
              { value: "en", label: "English" },
              { value: "zh", label: "简体中文" },
            ]}
          />
        </Row>
      </Group>
      <h3 className="mt-6 mb-2 text-sm font-medium text-muted-foreground">{t("settings.sidebarHistory")}</h3>
      <Group>
        <Row label={t("settings.sidebarScope")} hint={t("settings.sidebarScopeHint")}>
          <Segmented<SidebarScope>
            value={scope}
            onChange={setSidebarScope}
            options={[
              { value: "all", label: t("sidebar.scopeAll") },
              { value: "vetro", label: t("sidebar.scopeVetro") },
            ]}
          />
        </Row>
        <Row label={t("settings.activityWindow")} hint={t("settings.activityWindowHint")}>
          <ActivityWindow />
        </Row>
        <Row label={t("settings.hideDisabledHistory")} hint={t("settings.hideDisabledHistoryHint")}>
          <Switch checked={hideDisabled} aria-label={t("settings.hideDisabledHistory")} onCheckedChange={setHideDisabledAgentHistory} />
        </Row>
        <Row label={t("settings.showInternal")} hint={t("settings.showInternalHint")}>
          <Switch checked={showInternal} aria-label={t("settings.showInternal")} onCheckedChange={(checked) => actions.setFilters({ showInternal: checked })} />
        </Row>
      </Group>
      <WorkspaceSettings />
    </section>
  );
}

function AboutSection() {
  const t = useT();
  const app = useStore((s) => s.app);
  const { settings, update } = useSettings();
  const remoteClient = app?.remoteClient ?? false;
  const [idle, setIdle] = useState<string>("");
  useEffect(() => {
    if (settings) setIdle(String(settings.idleReleaseMinutes));
  }, [settings]);
  return (
    <section className="flex flex-col gap-6">
      <div>
        <SectionTitle title={t("settings.about")} />
        <Group>
          <Row label={t("settings.version")}>
            <span className="font-mono text-xs text-muted-foreground">{app?.version}</span>
          </Row>
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
          <Row label={t("settings.idleRelease")} hint={t("settings.idleReleaseHint")}>
            <Input
              type="number"
              min={0}
              disabled={remoteClient}
              value={idle}
              onChange={(event) => setIdle(event.target.value)}
              onBlur={() => {
                const minutes = Math.max(0, Math.round(Number(idle) || 0));
                if (settings && minutes !== settings.idleReleaseMinutes) void update({ idleReleaseMinutes: minutes });
              }}
              className="w-20 text-right tabular-nums"
            />
          </Row>
        </Group>
      </div>
      <div>
        <SectionTitle title={t("settings.rulesTitle")} />
        <p className="text-sm leading-relaxed text-muted-foreground">{t("settings.rules")}</p>
        <p className="mt-3 text-sm leading-relaxed text-muted-foreground">{t("settings.rawLog")}</p>
      </div>
    </section>
  );
}
