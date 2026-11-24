import { Dialog as DialogPrimitive } from "@base-ui/react/dialog";
import { CheckIcon, CopyIcon, DownloadIcon, FileUpIcon, PaintbrushIcon, PencilIcon, PlusIcon, Trash2Icon, TriangleAlertIcon, XIcon } from "lucide-react";
import { type CSSProperties, type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { type Translate, type MessageKey, useT } from "~/i18n";
import { cn, copyText } from "~/lib/utils";
import { clearThemePreview, deleteCustomTheme, preferredMode, previewThemeTokens, saveCustomTheme, setThemeId, usePrefs } from "~/state/prefs";
import { toast } from "~/state/toast";
import {
  BUILT_IN_THEMES,
  type BuiltInTheme,
  type CustomTheme,
  type SeedRole,
  type ThemeAppearance,
  type ThemeMode,
  type ThemePaletteTokens,
  SEED_ROLES,
  contrastRatio,
  customFromBuiltIn,
  customSeed,
  customTokens,
  exportThemeJson,
  isSafeColor,
  newThemeId,
  parseThemeJson,
  resolveColorHex,
  resolveTheme,
  themeMode,
  themeTokens,
  withSeedColor,
} from "~/styles/themes";
import { Button } from "../ui/button";
import { Dialog, DialogDescription, DialogPopup, DialogTitle, Tip } from "../ui/overlay";
import { Input, Textarea } from "../ui/primitives";
import { Segmented } from "../ui/segmented";

export function themeDisplayName(theme: BuiltInTheme | CustomTheme, t: Translate) {
  return "nameKey" in theme ? t(theme.nameKey) : theme.name;
}

/** The mode a gallery card or palette entry should show for a theme. */
export function usePreviewMode() {
  const theme = usePrefs((s) => s.theme);
  const systemDark = usePrefs((s) => s.systemDark);
  return preferredMode(theme, systemDark);
}

function cardTokens(theme: BuiltInTheme | CustomTheme, preferred: ThemeMode): ThemePaletteTokens {
  const resolved = "nameKey" in theme ? ({ kind: "builtin", theme } as const) : ({ kind: "custom", theme } as const);
  return themeTokens(resolved, themeMode(resolved, preferred));
}

/** A tiny two-tone dot: canvas with the accent in the corner. */
export function ThemeSwatchDot({ theme, mode, className }: { theme: BuiltInTheme | CustomTheme; mode: ThemeMode; className?: string }) {
  const tokens = cardTokens(theme, mode);
  return (
    <span className={cn("relative inline-flex size-4 shrink-0 overflow-hidden rounded-full border border-foreground/15", className)} style={{ background: tokens.background }} aria-hidden>
      <span className="absolute inset-y-0 left-0 w-1/3" style={{ background: tokens.sidebar }} />
      <span className="absolute right-0.5 bottom-0.5 size-1.5 rounded-full" style={{ background: tokens.primary }} />
    </span>
  );
}

function MiniApp({ tokens }: { tokens: ThemePaletteTokens }) {
  const bar = (color: string, width: string, extra?: CSSProperties) => <span className="block h-1 rounded-full" style={{ background: color, width, ...extra }} />;
  return (
    <div className="flex h-[4.5rem] w-full overflow-hidden" style={{ background: tokens.background }}>
      <div className="flex w-[28%] shrink-0 flex-col gap-1.5 border-e px-1.5 pt-2" style={{ background: tokens.sidebar, borderColor: tokens.sidebarBorder }}>
        {bar(tokens.sidebarForeground, "70%", { opacity: 0.75 })}
        <span className="block h-2 rounded-sm" style={{ background: tokens.sidebarRowActive }} />
        {bar(tokens.sidebarMutedForeground, "55%", { opacity: 0.6 })}
        {bar(tokens.sidebarMutedForeground, "62%", { opacity: 0.6 })}
      </div>
      <div className="flex min-w-0 flex-1 flex-col gap-1.5 px-2 pt-2">
        <span className="ms-auto block h-2.5 w-1/2 rounded-full" style={{ background: tokens.messageSurface }} />
        {bar(tokens.foreground, "78%", { opacity: 0.85 })}
        {bar(tokens.mutedForeground, "56%", { opacity: 0.7 })}
        <span className="mt-auto mb-1.5 flex h-3 items-center justify-end rounded-[4px] border px-0.5" style={{ background: tokens.card, borderColor: tokens.border }}>
          <span className="size-2 rounded-full" style={{ background: tokens.primary }} />
        </span>
      </div>
    </div>
  );
}

function CardAction({ label, onClick, children, tone }: { label: string; onClick: () => void; children: ReactNode; tone?: "danger" }) {
  return (
    <Tip label={label}>
      <button
        type="button"
        aria-label={label}
        onClick={onClick}
        className={cn(
          "inline-flex size-6 items-center justify-center rounded-md bg-popover/90 text-muted-foreground shadow-xs ring-1 ring-foreground/10 backdrop-blur hover:text-foreground [&_svg]:size-3.5",
          tone === "danger" && "bg-destructive text-white ring-destructive hover:text-white",
        )}
      >
        {children}
      </button>
    </Tip>
  );
}

function ThemeCard({ theme, active, mode, actions }: { theme: BuiltInTheme | CustomTheme; active: boolean; mode: ThemeMode; actions: ReactNode }) {
  const t = useT();
  const tokens = cardTokens(theme, mode);
  const name = themeDisplayName(theme, t);
  const single = !("nameKey" in theme) && theme.appearance !== "both" ? (theme.appearance === "light" ? t("theme.lightOnly") : t("theme.darkOnly")) : null;
  return (
    <div className="group/theme relative">
      <button type="button" aria-pressed={active} onClick={() => setThemeId(theme.id)} className="flex w-full flex-col gap-1.5 rounded-xl text-left outline-none focus-visible:[&>div]:ring-2 focus-visible:[&>div]:ring-ring/60">
        <div className={cn("w-full overflow-hidden rounded-lg border transition-shadow", active ? "ring-2 ring-ring ring-offset-2 ring-offset-background" : "group-hover/theme:border-foreground/25")}>
          <MiniApp tokens={tokens} />
        </div>
        <span className="flex min-w-0 items-center gap-1.5 px-0.5 text-xs">
          <span className={cn("truncate", active ? "font-medium text-foreground" : "text-foreground/80")}>{name}</span>
          {single ? <span className="shrink-0 text-[10px] text-muted-foreground">{single}</span> : null}
          {active ? <CheckIcon className="ms-auto size-3.5 shrink-0 text-primary" aria-label={t("theme.active")} /> : null}
        </span>
      </button>
      <div className="absolute top-1.5 right-1.5 flex gap-1 opacity-0 transition-opacity group-focus-within/theme:opacity-100 group-hover/theme:opacity-100 pointer-coarse:opacity-100">{actions}</div>
    </div>
  );
}

function GalleryGroup({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-2">
      <div className="text-xs font-medium text-muted-foreground">{title}</div>
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">{children}</div>
    </div>
  );
}

function exportTheme(theme: CustomTheme | BuiltInTheme, t: Translate) {
  void copyText(exportThemeJson(theme)).then(() => toast.success(t("theme.exported")));
}

export function ThemeGallery() {
  const t = useT();
  const themeId = usePrefs((s) => s.themeId);
  const custom = usePrefs((s) => s.customThemes);
  const mode = usePreviewMode();
  const [editing, setEditing] = useState<CustomTheme | null>(null);
  const [importOpen, setImportOpen] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const activeId = resolveTheme(themeId, custom).theme.id;

  const startNew = () => {
    const base = resolveTheme(themeId, custom);
    if (base.kind === "builtin") {
      setEditing(customFromBuiltIn(base.theme, t("theme.untitled")));
      return;
    }
    setEditing({
      ...base.theme,
      id: newThemeId(),
      name: t("theme.untitled"),
      light: customSeed(base.theme, "light"),
      dark: customSeed(base.theme, "dark"),
    });
  };

  const paper = BUILT_IN_THEMES.filter((theme) => theme.category === "paper");
  const classics = BUILT_IN_THEMES.filter((theme) => theme.category === "t3");
  const builtInActions = (theme: BuiltInTheme) => (
    <CardAction label={t("theme.customize")} onClick={() => setEditing(customFromBuiltIn(theme, t("theme.copyName", { name: t(theme.nameKey) })))}>
      <PaintbrushIcon />
    </CardAction>
  );

  return (
    <>
      <div className="mt-6 mb-2 flex items-center justify-between gap-3">
        <h3 className="text-sm font-medium text-muted-foreground">{t("settings.themes")}</h3>
        <div className="flex items-center gap-1.5">
          <Button size="xs" variant="ghost-muted" onClick={() => setImportOpen(true)}>
            <FileUpIcon />
            {t("theme.import")}
          </Button>
          <Button size="xs" variant="outline" onClick={startNew}>
            <PlusIcon />
            {t("theme.new")}
          </Button>
        </div>
      </div>
      <div className="flex flex-col gap-5 rounded-xl border bg-card/80 p-4">
        <GalleryGroup title={t("theme.category.paper")}>
          {paper.map((theme) => (
            <ThemeCard key={theme.id} theme={theme} active={activeId === theme.id} mode={mode} actions={builtInActions(theme)} />
          ))}
        </GalleryGroup>
        <GalleryGroup title={t("theme.category.t3")}>
          {classics.map((theme) => (
            <ThemeCard key={theme.id} theme={theme} active={activeId === theme.id} mode={mode} actions={builtInActions(theme)} />
          ))}
        </GalleryGroup>
        {custom.length > 0 ? (
          <GalleryGroup title={t("theme.category.custom")}>
            {custom.map((theme) => (
              <ThemeCard
                key={theme.id}
                theme={theme}
                active={activeId === theme.id}
                mode={mode}
                actions={
                  <>
                    <CardAction label={t("theme.edit")} onClick={() => setEditing(theme)}>
                      <PencilIcon />
                    </CardAction>
                    <CardAction label={t("theme.export")} onClick={() => exportTheme(theme, t)}>
                      <DownloadIcon />
                    </CardAction>
                    <span onMouseLeave={() => setConfirmDelete(null)} className="contents">
                      <CardAction
                        label={confirmDelete === theme.id ? t("theme.deleteConfirm") : t("theme.delete")}
                        tone={confirmDelete === theme.id ? "danger" : undefined}
                        onClick={() => {
                          if (confirmDelete !== theme.id) {
                            setConfirmDelete(theme.id);
                            return;
                          }
                          setConfirmDelete(null);
                          deleteCustomTheme(theme.id);
                          toast.info(t("theme.deleted", { name: theme.name }));
                        }}
                      >
                        <Trash2Icon />
                      </CardAction>
                    </span>
                  </>
                }
              />
            ))}
          </GalleryGroup>
        ) : null}
      </div>
      {editing ? <ThemeEditorDialog key={editing.id} initial={editing} onClose={() => setEditing(null)} /> : null}
      <ThemeImportDialog open={importOpen} onOpenChange={setImportOpen} />
    </>
  );
}

const ACCENTS: { id: "cobalt" | "terracotta" | "amber" | "sage" | "teal" | "rose" | "violet" | "slate"; hex: string }[] = [
  { id: "cobalt", hex: "#2563eb" },
  { id: "terracotta", hex: "#c96442" },
  { id: "amber", hex: "#b46a1f" },
  { id: "sage", hex: "#2f855a" },
  { id: "teal", hex: "#0d9488" },
  { id: "rose", hex: "#be4b6b" },
  { id: "violet", hex: "#7c3aed" },
  { id: "slate", hex: "#475569" },
];

function mixHex(a: string, b: string, share: number) {
  const parse = (hex: string) => [1, 3, 5].map((i) => Number.parseInt(hex.slice(i, i + 2), 16));
  const [x, y] = [parse(a), parse(b)];
  return `#${x.map((channel, i) => Math.round(channel * (1 - share) + y[i]! * share).toString(16).padStart(2, "0")).join("")}`;
}

/** Accents tuned for light canvases get lifted a little on dark ones so they keep their contrast. */
function accentFor(hex: string, mode: ThemeMode) {
  return mode === "dark" ? mixHex(hex, "#ffffff", 0.22) : hex;
}

function ColorField({ role, value, onChange }: { role: SeedRole; value: string; onChange: (color: string) => void }) {
  const t = useT();
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  const valid = isSafeColor(draft);
  const hex = resolveColorHex(value) ?? "#000000";
  const label = t(`theme.role.${role}` as MessageKey);
  return (
    <label className="flex items-center gap-3">
      <span className="min-w-0 flex-1 truncate text-sm text-foreground">{label}</span>
      <span className="relative size-7 shrink-0 overflow-hidden rounded-md border border-foreground/15 shadow-xs" style={{ background: value }}>
        <input
          type="color"
          value={hex}
          aria-label={label}
          onChange={(event) => onChange(event.target.value)}
          className="absolute inset-0 size-full cursor-pointer opacity-0"
        />
      </span>
      <Input
        value={draft}
        spellCheck={false}
        aria-invalid={!valid || undefined}
        title={valid ? undefined : t("theme.editor.invalidColor")}
        onChange={(event) => {
          setDraft(event.target.value);
          if (isSafeColor(event.target.value)) onChange(event.target.value.trim());
        }}
        onBlur={() => setDraft(value)}
        className={cn("h-7 w-28 font-mono text-xs sm:h-7 sm:text-xs", !valid && "border-destructive/60 ring-destructive/30")}
      />
    </label>
  );
}

function finalTheme(draft: CustomTheme, name: string): CustomTheme {
  const keep = (mode: ThemeMode) => draft.appearance === "both" || draft.appearance === mode;
  return {
    id: draft.id,
    name: name.trim() || draft.name,
    appearance: draft.appearance,
    ...(keep("light") ? { light: customSeed(draft, "light") } : {}),
    ...(keep("dark") ? { dark: customSeed(draft, "dark") } : {}),
    overrides: {
      ...(keep("light") && draft.overrides?.light ? { light: draft.overrides.light } : {}),
      ...(keep("dark") && draft.overrides?.dark ? { dark: draft.overrides.dark } : {}),
    },
  };
}

/** Edits a custom theme in a side panel while the real app behind it previews every change. */
export function ThemeEditorDialog({ initial, onClose }: { initial: CustomTheme; onClose: () => void }) {
  const t = useT();
  const preferred = usePreviewMode();
  const [draft, setDraft] = useState<CustomTheme>(() => ({
    ...initial,
    light: customSeed(initial, "light"),
    dark: customSeed(initial, "dark"),
  }));
  const [name, setName] = useState(initial.name);
  const nameRef = useRef<HTMLInputElement>(null);
  const [variant, setVariant] = useState<ThemeMode>(initial.appearance === "both" ? preferred : initial.appearance);
  const mode: ThemeMode = draft.appearance === "both" ? variant : draft.appearance;
  const tokens = useMemo(() => customTokens(draft, mode), [draft, mode]);
  const seed = customSeed(draft, mode);

  useEffect(() => {
    previewThemeTokens(mode, tokens);
  }, [mode, tokens]);
  useEffect(() => clearThemePreview, []);

  const close = () => {
    clearThemePreview();
    onClose();
  };

  const save = () => {
    const theme = finalTheme(draft, name);
    clearThemePreview();
    saveCustomTheme(theme);
    setThemeId(theme.id);
    toast.success(t("theme.saved", { name: theme.name }));
    onClose();
  };

  const setColor = (role: SeedRole, color: string) => setDraft((current) => withSeedColor(current, mode, role, color));
  const fg = resolveColorHex(tokens.foreground);
  const bg = resolveColorHex(tokens.background);
  const ratio = fg && bg ? contrastRatio(fg, bg) : null;

  return (
    <DialogPrimitive.Root open onOpenChange={(open) => (open ? undefined : close())}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Backdrop className="fixed inset-0 z-50 bg-transparent" />
        <DialogPrimitive.Popup initialFocus={nameRef} className="dialog-glass fixed inset-x-2 bottom-2 z-50 flex max-h-[min(40rem,calc(100dvh-1rem))] flex-col overflow-hidden rounded-2xl border text-popover-foreground outline-none transition-[translate,opacity] duration-200 ease-(--ease-drawer) data-ending-style:translate-y-4 data-ending-style:opacity-0 data-starting-style:translate-y-4 data-starting-style:opacity-0 sm:inset-x-auto sm:top-[calc(var(--workspace-topbar-height)+0.5rem)] sm:right-3 sm:bottom-3 sm:max-h-none sm:w-[23rem] sm:data-ending-style:translate-x-4 sm:data-ending-style:translate-y-0 sm:data-starting-style:translate-x-4 sm:data-starting-style:translate-y-0">
          <div className="flex shrink-0 items-start gap-3 border-b px-4 py-3">
            <div className="min-w-0 flex-1">
              <DialogPrimitive.Title className="text-sm font-semibold">{t("theme.editor.title")}</DialogPrimitive.Title>
              <DialogPrimitive.Description className="mt-0.5 text-xs text-muted-foreground">{t("theme.editor.hint")}</DialogPrimitive.Description>
            </div>
            <Button size="icon-xs" variant="ghost-muted" aria-label={t("common.close")} onClick={close}>
              <XIcon />
            </Button>
          </div>
          <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-4 py-4">
            <label className="flex flex-col gap-1.5">
              <span className="text-xs font-medium text-muted-foreground">{t("theme.editor.name")}</span>
              <Input ref={nameRef} value={name} maxLength={60} onChange={(event) => setName(event.target.value)} />
            </label>
            <div className="flex flex-col gap-1.5">
              <span className="text-xs font-medium text-muted-foreground">{t("theme.editor.appearance")}</span>
              <Segmented<ThemeAppearance>
                className="[&>button]:flex-1"
                label={t("theme.editor.appearance")}
                value={draft.appearance}
                onChange={(appearance) => {
                  setDraft((current) => ({ ...current, appearance }));
                  if (appearance !== "both") setVariant(appearance);
                }}
                options={[
                  { value: "light", label: t("settings.themeLight") },
                  { value: "dark", label: t("settings.themeDark") },
                  { value: "both", label: t("theme.editor.both") },
                ]}
              />
            </div>
            {draft.appearance === "both" ? (
              <div className="flex items-center justify-between gap-3">
                <span className="text-xs font-medium text-muted-foreground">{t("theme.editor.variant")}</span>
                <Segmented<ThemeMode>
                  label={t("theme.editor.variant")}
                  value={variant}
                  onChange={setVariant}
                  options={[
                    { value: "light", label: t("settings.themeLight") },
                    { value: "dark", label: t("settings.themeDark") },
                  ]}
                />
              </div>
            ) : null}
            <div className="flex flex-col gap-2">
              <span className="text-xs font-medium text-muted-foreground">{t("theme.editor.accents")}</span>
              <div className="flex flex-wrap gap-2">
                {ACCENTS.map((accent) => {
                  const color = accentFor(accent.hex, mode);
                  const selected = resolveColorHex(seed.primary) === color;
                  return (
                    <Tip key={accent.id} label={t(`theme.accent.${accent.id}`)}>
                      <button
                        type="button"
                        aria-label={t(`theme.accent.${accent.id}`)}
                        aria-pressed={selected}
                        onClick={() => setColor("primary", color)}
                        className={cn(
                          "size-6 rounded-full border border-foreground/15 shadow-xs transition-transform hover:scale-110",
                          selected && "ring-2 ring-ring ring-offset-2 ring-offset-popover",
                        )}
                        style={{ background: color }}
                      />
                    </Tip>
                  );
                })}
              </div>
            </div>
            <div className="flex flex-col gap-2.5">
              <span className="text-xs font-medium text-muted-foreground">{t("theme.editor.colors")}</span>
              {SEED_ROLES.map((role) => (
                <ColorField key={`${mode}:${role}`} role={role} value={seed[role]} onChange={(color) => setColor(role, color)} />
              ))}
              {ratio !== null && ratio < 4.5 ? (
                <p className="flex items-start gap-1.5 rounded-md bg-warning-surface px-2 py-1.5 text-xs text-warning-foreground">
                  <TriangleAlertIcon className="mt-px size-3.5 shrink-0" />
                  {t("theme.editor.lowContrast", { ratio: ratio.toFixed(1) })}
                </p>
              ) : null}
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-2 border-t px-4 py-3">
            <Button size="sm" variant="ghost-muted" onClick={() => void copyText(exportThemeJson(finalTheme(draft, name))).then(() => toast.success(t("theme.exported")))}>
              <CopyIcon />
              {t("theme.editor.copyJson")}
            </Button>
            <span className="flex-1" />
            <Button size="sm" variant="ghost" onClick={close}>
              {t("common.cancel")}
            </Button>
            <Button size="sm" onClick={save}>
              {t("common.save")}
            </Button>
          </div>
        </DialogPrimitive.Popup>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

function importError(code: string, t: Translate) {
  if (code.startsWith("invalid:")) return t("theme.error.invalid", { keys: code.slice("invalid:".length) });
  if (code === "json" || code === "shape" || code === "version" || code === "missing") return t(`theme.error.${code}`);
  return t("theme.error.shape");
}

export function ThemeImportDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const t = useT();
  const [text, setText] = useState("");
  const fileRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (!open) setText("");
  }, [open]);
  const result = useMemo(() => (text.trim() ? parseThemeJson(text) : null), [text]);

  const submit = () => {
    if (!result?.ok) return;
    saveCustomTheme(result.theme);
    setThemeId(result.theme.id);
    toast.success(t("theme.imported", { name: result.theme.name }));
    onOpenChange(false);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogPopup className="max-w-lg">
        <form
          className="flex flex-col gap-4 p-5"
          onSubmit={(event) => {
            event.preventDefault();
            submit();
          }}
        >
          <div className="flex flex-col gap-1 pe-8">
            <DialogTitle>{t("theme.importTitle")}</DialogTitle>
            <DialogDescription>{t("theme.importHint")}</DialogDescription>
          </div>
          <Textarea
            autoFocus
            rows={10}
            spellCheck={false}
            value={text}
            placeholder={'{\n  "version": 1,\n  "name": "…",\n  "colors": { "canvas": "#faf7f0", "text": "#2b2723", "accent": "#c96442" }\n}'}
            onChange={(event) => setText(event.target.value)}
            className="font-mono text-xs sm:text-xs"
          />
          {result && !result.ok ? <p className="-mt-2 text-xs text-destructive-foreground">{importError(result.error, t)}</p> : null}
          <input
            ref={fileRef}
            type="file"
            accept=".json,application/json"
            className="hidden"
            onChange={(event) => {
              const file = event.target.files?.[0];
              event.target.value = "";
              if (file) void file.text().then(setText);
            }}
          />
          <div className="flex items-center gap-2">
            <Button type="button" size="sm" variant="outline" onClick={() => fileRef.current?.click()}>
              <FileUpIcon />
              {t("theme.importFile")}
            </Button>
            <span className="flex-1" />
            <Button type="button" size="sm" variant="ghost" onClick={() => onOpenChange(false)}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" size="sm" disabled={!result?.ok}>
              {t("theme.importSubmit")}
            </Button>
          </div>
        </form>
      </DialogPopup>
    </Dialog>
  );
}

