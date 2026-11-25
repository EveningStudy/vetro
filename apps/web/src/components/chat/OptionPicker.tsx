import { BrainIcon, CheckIcon, ChevronDownIcon, CpuIcon, RotateCcwIcon, SearchIcon, ShieldAlertIcon, ShieldIcon, SlidersHorizontalIcon } from "lucide-react";
import { useMemo, useRef, useState } from "react";
import type { ConfigOption } from "~/api/types";
import { useT } from "~/i18n";
import { cn } from "~/lib/utils";
import {
  Menu,
  MenuGroupLabel,
  MenuItem,
  MenuPopup,
  MenuRadioGroup,
  MenuRadioItem,
  MenuSeparator,
  MenuTrigger,
  Popover,
  PopoverPopup,
  PopoverTrigger,
} from "../ui/overlay";

const categoryIcons = {
  model: CpuIcon,
  thought_level: BrainIcon,
  mode: ShieldIcon,
  other: SlidersHorizontalIcon,
} as const;

const SEARCHABLE_AFTER = 12;

// Trigger styling follows T3 Code's ComposerControl (MIT).
export const composerControlClass =
  "relative inline-flex h-7 shrink-0 cursor-pointer items-center justify-center gap-1.5 whitespace-nowrap rounded-(--control-radius) border border-transparent px-2 font-medium text-foreground/80 text-sm outline-none hover:bg-accent hover:text-foreground data-popup-open:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-64 sm:text-[13px] [&_svg]:shrink-0 [&_svg:not([class*='text-'])]:text-muted-foreground [&_svg:not([class*='size-'])]:size-4 sm:[&_svg:not([class*='size-'])]:size-3.5";

type Choice = ConfigOption["choices"][number] & { label: string; section: string };

const DANGER_TERMS = [
  "bypass",
  "yolo",
  "danger",
  "full-access",
  "full_access",
  "full access",
  "unrestricted",
  "autonomous",
  "dontask",
  "dont-ask",
  "skip-permissions",
  "skip_permissions",
  "trust-all",
  "allow-all",
];

/** Mirrors the core's danger rules so a mode that relaxes safeguards is flagged even when an
 * agent (or an older core) did not mark it. */
export function isDangerChoice(option: Pick<ConfigOption, "id" | "name" | "category">, choice: ConfigOption["choices"][number]) {
  if (choice.danger) return true;
  const key = `${option.id} ${option.name}`.toLowerCase();
  const governs = option.category === "mode" || /mode|perm|approval|sandbox|autopilot|trust|safety|policy/.test(key);
  if (!governs) return false;
  const texts = [choice.value, choice.name].map((text) => text.trim().toLowerCase());
  if (key.includes("approval") && texts[0] === "never") return true;
  if (key.includes("autopilot") && texts.some((text) => ["on", "true", "enabled", "always"].includes(text))) return true;
  return texts.some((text) => DANGER_TERMS.some((term) => text.includes(term)));
}

/** Some agents report a choice only by its id ("acceptEdits", "xhigh"). Show that id as words; the value is untouched. */
function readable(choice: ConfigOption["choices"][number]): string {
  if (choice.name !== choice.value || !/^[A-Za-z][A-Za-z0-9_-]*$/.test(choice.name)) return choice.name;
  const words = choice.name.replace(/([a-z0-9])([A-Z])/g, "$1 $2").replace(/[_-]+/g, " ").toLowerCase();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** Agents report choices with their own labels. A shared " · Provider" suffix becomes a group. */
export function prepare(option: ConfigOption): Choice[] {
  const suffixed = option.choices.length > 1 && option.choices.every((choice) => / · [^·]+$/.test(choice.name));
  return option.choices.map((choice) => {
    const danger = isDangerChoice(option, choice);
    if (choice.group || !suffixed) return { ...choice, danger, label: readable(choice), section: choice.group ?? "" };
    const index = choice.name.lastIndexOf(" · ");
    return { ...choice, danger, label: choice.name.slice(0, index), section: choice.name.slice(index + 3) };
  });
}

function sections(choices: Choice[]) {
  const map = new Map<string, Choice[]>();
  for (const choice of choices) map.set(choice.section, [...(map.get(choice.section) ?? []), choice]);
  return [...map.entries()];
}

/** Renders any agent-reported setting the same way: the agent decides which ones exist. */
export function OptionPicker({
  option,
  onChange,
  defaultValue,
  disabled,
  compact,
}: {
  option: ConfigOption;
  onChange: (value: string) => void;
  defaultValue?: string;
  disabled?: boolean;
  compact?: boolean;
}) {
  const t = useT();
  const choices = useMemo(() => prepare(option), [option]);
  const current = choices.find((choice) => choice.value === option.currentValue);
  const danger = !!current?.danger;
  const Icon = danger ? ShieldAlertIcon : categoryIcons[option.category];
  const label = current?.label || option.name;
  const triggerClass = cn(
    composerControlClass,
    danger ? "shrink-0" : option.category === "model" ? "min-w-[4.5rem] max-w-[11rem] shrink" : "max-w-[9rem] @max-[36rem]:px-1.5",
    danger &&
      "border-destructive/40 bg-error-surface/70 text-destructive-foreground hover:bg-error-surface hover:text-destructive-foreground [&_svg:not([class*='text-'])]:text-destructive-foreground",
  );
  const trigger = (
    <>
      {option.category !== "model" || compact || danger ? <Icon /> : null}
      <span className={cn("min-w-0 max-w-44 truncate", !current && "text-muted-foreground")}>{label}</span>
      <ChevronDownIcon className="-me-1 size-3 opacity-70" />
    </>
  );
  if (choices.length > SEARCHABLE_AFTER)
    return <SearchablePicker option={option} choices={choices} onChange={onChange} disabled={disabled} trigger={trigger} triggerClass={triggerClass} />;
  const safeDefault = defaultValue ?? choices.find((choice) => !choice.danger)?.value;
  const resettable = danger && !!safeDefault && safeDefault !== option.currentValue && choices.some((choice) => choice.value === safeDefault);
  return (
    <Menu>
      <MenuTrigger disabled={disabled} className={triggerClass} aria-label={option.name} title={option.description ?? option.name}>
        {trigger}
      </MenuTrigger>
      <MenuPopup side="top" align="start" className="w-64">
        <MenuGroupLabel>{option.name}</MenuGroupLabel>
        <MenuRadioGroup value={option.currentValue} onValueChange={(value) => onChange(String(value))}>
          {sections(choices).map(([section, list], index) => (
            <div key={section || index}>
              {index > 0 ? <MenuSeparator /> : null}
              {section ? <MenuGroupLabel className="pt-1 pb-0.5 text-2xs uppercase tracking-wide">{section}</MenuGroupLabel> : null}
              {list.map((choice) => (
                <MenuRadioItem key={choice.value} value={choice.value} className="items-start py-1.5">
                  <span className="flex min-w-0 flex-col">
                    <span className={cn("inline-flex items-center gap-1.5 truncate", choice.danger && "text-destructive-foreground")}>
                      {choice.danger ? <ShieldAlertIcon className="size-3.5 shrink-0 text-destructive-foreground" /> : null}
                      <span className="truncate">{choice.label}</span>
                    </span>
                    {choice.description ? <span className="text-xs text-muted-foreground">{choice.description}</span> : null}
                  </span>
                </MenuRadioItem>
              ))}
            </div>
          ))}
        </MenuRadioGroup>
        {resettable ? (
          <>
            <MenuSeparator />
            <MenuItem onClick={() => onChange(safeDefault!)}>
              <RotateCcwIcon />
              {t("composer.resetDefault")}
            </MenuItem>
          </>
        ) : null}
      </MenuPopup>
    </Menu>
  );
}

function SearchablePicker({
  option,
  choices,
  onChange,
  disabled,
  trigger,
  triggerClass,
}: {
  option: ConfigOption;
  choices: Choice[];
  onChange: (value: string) => void;
  disabled?: boolean;
  trigger: React.ReactNode;
  triggerClass: string;
}) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
  const filtered = choices.filter((choice) => {
    const haystack = `${choice.label} ${choice.value} ${choice.section}`.toLowerCase();
    return terms.every((term) => haystack.includes(term));
  });
  const grouped = sections(filtered);
  const ordered = grouped.flatMap(([, list]) => list);
  const pick = (choice: Choice | undefined) => {
    if (!choice) return;
    onChange(choice.value);
    setOpen(false);
  };
  let index = -1;
  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) {
          setQuery("");
          setActive(Math.max(0, sections(choices).flatMap(([, list]) => list).findIndex((choice) => choice.value === option.currentValue)));
        }
      }}
    >
      <PopoverTrigger disabled={disabled} className={triggerClass} aria-label={option.name} title={option.description ?? option.name}>
        {trigger}
      </PopoverTrigger>
      <PopoverPopup side="top" align="start" className="w-72" initialFocus={inputRef}>
        <div className="flex items-center gap-2 border-b px-3">
          <SearchIcon className="size-3.5 shrink-0 text-muted-foreground" />
          <input
            ref={inputRef}
            value={query}
            placeholder={`${option.name}…`}
            aria-label={t("sidebar.search")}
            onChange={(event) => {
              setQuery(event.target.value);
              setActive(0);
            }}
            onKeyDown={(event) => {
              if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                event.preventDefault();
                const delta = event.key === "ArrowDown" ? 1 : -1;
                const next = (active + delta + ordered.length) % Math.max(1, ordered.length);
                setActive(next);
                listRef.current?.querySelector(`[data-index="${next}"]`)?.scrollIntoView({ block: "nearest" });
              } else if (event.key === "Enter") {
                event.preventDefault();
                pick(ordered[active]);
              }
            }}
            className="h-9 min-w-0 flex-1 bg-transparent text-sm outline-none placeholder:text-placeholder"
          />
        </div>
        <div ref={listRef} role="listbox" aria-label={option.name} className="max-h-[min(22rem,var(--available-height))] overflow-y-auto p-1">
          {filtered.length === 0 ? <div className="px-2 py-1.5 text-sm text-muted-foreground">{t("palette.empty")}</div> : null}
          {grouped.map(([section, list]) => (
            <div key={section}>
              {section ? <div className="px-2 pt-1.5 pb-0.5 text-2xs font-medium uppercase tracking-wide text-muted-foreground">{section}</div> : null}
              {list.map((choice) => {
                index += 1;
                const current = index;
                const selected = choice.value === option.currentValue;
                return (
                  <button
                    key={choice.value}
                    type="button"
                    role="option"
                    aria-selected={selected}
                    data-index={current}
                    onMouseMove={() => setActive(current)}
                    onClick={() => pick(choice)}
                    className={cn("flex min-h-7 w-full items-center gap-2 rounded-sm px-2 py-1 text-left text-sm", current === active && "bg-accent")}
                  >
                    {choice.danger ? <ShieldAlertIcon className="size-3.5 shrink-0 text-destructive-foreground" /> : null}
                    <span className={cn("min-w-0 flex-1 truncate", choice.danger && "text-destructive-foreground")}>{choice.label}</span>
                    {selected ? <CheckIcon className="size-3.5 shrink-0" /> : null}
                  </button>
                );
              })}
            </div>
          ))}
        </div>
      </PopoverPopup>
    </Popover>
  );
}
