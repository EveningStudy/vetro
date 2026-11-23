import { BrainIcon, ChevronDownIcon, CpuIcon, ShieldIcon, SlidersHorizontalIcon } from "lucide-react";
import type { ConfigOption } from "~/api/types";
import { cn } from "~/lib/utils";
import { Menu, MenuGroupLabel, MenuPopup, MenuRadioGroup, MenuRadioItem, MenuSeparator, MenuTrigger } from "../ui/overlay";

const categoryIcons = {
  model: CpuIcon,
  thought_level: BrainIcon,
  mode: ShieldIcon,
  other: SlidersHorizontalIcon,
} as const;

// Trigger styling follows T3 Code's ComposerControl (MIT).
export const composerControlClass =
  "relative inline-flex h-7 shrink-0 cursor-pointer items-center justify-center gap-1.5 whitespace-nowrap rounded-(--control-radius) border border-transparent px-2 font-medium text-foreground/80 text-sm outline-none hover:bg-accent hover:text-foreground data-popup-open:bg-accent focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-64 sm:text-[13px] [&_svg]:shrink-0 [&_svg:not([class*='text-'])]:text-muted-foreground [&_svg:not([class*='size-'])]:size-4 sm:[&_svg:not([class*='size-'])]:size-3.5";

/** Renders any agent-reported setting the same way: the agent decides which ones exist. */
export function OptionPicker({
  option,
  onChange,
  disabled,
  compact,
}: {
  option: ConfigOption;
  onChange: (value: string) => void;
  disabled?: boolean;
  compact?: boolean;
}) {
  const current = option.choices.find((choice) => choice.value === option.currentValue);
  const Icon = categoryIcons[option.category];
  const groups = new Map<string, typeof option.choices>();
  for (const choice of option.choices) {
    const key = choice.group ?? "";
    groups.set(key, [...(groups.get(key) ?? []), choice]);
  }
  return (
    <Menu>
      <MenuTrigger disabled={disabled} className={composerControlClass} aria-label={option.name} title={option.description ?? option.name}>
        {option.category !== "model" || compact ? <Icon /> : null}
        <span className={cn(compact && "max-sm:hidden", "max-w-40 truncate")}>{current?.name ?? option.currentValue}</span>
        <ChevronDownIcon className="-me-1 size-3 opacity-70" />
      </MenuTrigger>
      <MenuPopup side="top" align="start" className="w-64">
        <MenuGroupLabel>{option.name}</MenuGroupLabel>
        <MenuRadioGroup value={option.currentValue} onValueChange={(value) => onChange(String(value))}>
          {[...groups.entries()].map(([group, choices], index) => (
            <div key={group || index}>
              {index > 0 ? <MenuSeparator /> : null}
              {group ? <MenuGroupLabel className="pt-1 pb-0.5 text-2xs uppercase tracking-wide">{group}</MenuGroupLabel> : null}
              {choices.map((choice) => (
                <MenuRadioItem key={choice.value} value={choice.value} className="items-start py-1.5">
                  <span className="flex min-w-0 flex-col">
                    <span className="truncate">{choice.name}</span>
                    {choice.description ? <span className="text-xs text-muted-foreground">{choice.description}</span> : null}
                  </span>
                </MenuRadioItem>
              ))}
            </div>
          ))}
        </MenuRadioGroup>
      </MenuPopup>
    </Menu>
  );
}
