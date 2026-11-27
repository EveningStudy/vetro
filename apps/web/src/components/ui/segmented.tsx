import type { LucideIcon } from "lucide-react";
import { useRef } from "react";
import { cn } from "~/lib/utils";

export function Segmented<T extends string>({
  value,
  options,
  onChange,
  disabled,
  className,
  label,
}: {
  value: T;
  options: { value: T; label: string; icon?: LucideIcon }[];
  onChange: (value: T) => void;
  disabled?: boolean;
  className?: string;
  label?: string;
}) {
  const buttonsRef = useRef<(HTMLButtonElement | null)[]>([]);
  const move = (index: number, delta: number) => {
    if (disabled || options.length === 0) return;
    const next = (index + delta + options.length) % options.length;
    onChange(options[next]!.value);
    buttonsRef.current[next]?.focus();
  };

  return (
    <div className={cn("flex rounded-lg bg-input/40 p-0.5", disabled && "opacity-60", className)} role="radiogroup" aria-label={label}>
      {options.map((option, index) => {
        const selected = value === option.value;
        return (
          <button
            key={option.value}
            ref={(el) => {
              buttonsRef.current[index] = el;
            }}
            type="button"
            role="radio"
            disabled={disabled}
            tabIndex={disabled ? -1 : selected ? 0 : -1}
            aria-checked={selected}
            onClick={() => onChange(option.value)}
            onKeyDown={(event) => {
              if (event.key === "ArrowRight" || event.key === "ArrowDown") {
                event.preventDefault();
                move(index, 1);
              } else if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
                event.preventDefault();
                move(index, -1);
              } else if (event.key === "Home") {
                event.preventDefault();
                move(0, 0);
              } else if (event.key === "End") {
                event.preventDefault();
                move(options.length - 1, 0);
              }
            }}
            className={cn(
              "flex h-7 items-center justify-center gap-1.5 rounded-md px-2.5 text-xs font-medium whitespace-nowrap text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none",
              selected && "bg-background text-foreground shadow-xs dark:bg-input/72",
            )}
          >
            {option.icon ? <option.icon className="size-3.5" /> : null}
            {option.label}
          </button>
        );
      })}
    </div>
  );
}
