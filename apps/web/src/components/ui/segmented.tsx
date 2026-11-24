import type { LucideIcon } from "lucide-react";
import { cn } from "~/lib/utils";

export function Segmented<T extends string>({
  value,
  options,
  onChange,
  className,
  label,
}: {
  value: T;
  options: { value: T; label: string; icon?: LucideIcon }[];
  onChange: (value: T) => void;
  className?: string;
  label?: string;
}) {
  return (
    <div className={cn("flex rounded-lg bg-input/40 p-0.5", className)} role="radiogroup" aria-label={label}>
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          role="radio"
          aria-checked={value === option.value}
          onClick={() => onChange(option.value)}
          className={cn(
            "flex h-7 items-center justify-center gap-1.5 rounded-md px-2.5 text-xs font-medium whitespace-nowrap text-muted-foreground hover:text-foreground",
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
