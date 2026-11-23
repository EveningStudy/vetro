// Badge, Kbd, Spinner, Switch, Input and Textarea adapted from T3 Code's components/ui (MIT).
import { Switch as SwitchPrimitive } from "@base-ui/react/switch";
import { cva, type VariantProps } from "class-variance-authority";
import { LoaderCircleIcon } from "lucide-react";
import type * as React from "react";
import { cn } from "~/lib/utils";

const badgeVariants = cva(
  "relative inline-flex shrink-0 items-center justify-center gap-1 whitespace-nowrap rounded-sm border border-transparent font-medium [&_svg:not([class*='size-'])]:size-3 [&_svg]:shrink-0",
  {
    defaultVariants: { size: "default", variant: "secondary" },
    variants: {
      size: {
        default: "h-5 min-w-5 px-1.5 text-xs sm:h-4.5 sm:text-2xs",
        sm: "h-4 min-w-4 rounded-[.25rem] px-1 text-3xs leading-none",
      },
      variant: {
        default: "bg-primary text-primary-foreground",
        error: "bg-destructive/8 text-destructive-foreground dark:bg-destructive/16",
        info: "bg-info/8 text-info-foreground dark:bg-info/16",
        outline: "border-border bg-transparent text-muted-foreground",
        secondary: "bg-foreground/[0.06] text-secondary-foreground dark:bg-foreground/[0.08]",
        success: "bg-success/8 text-success-foreground dark:bg-success/16",
        warning: "bg-warning/8 text-warning-foreground dark:bg-warning/16",
      },
    },
  },
);

export function Badge({
  className,
  variant,
  size,
  ...props
}: React.ComponentProps<"span"> & VariantProps<typeof badgeVariants>) {
  return <span className={cn(badgeVariants({ variant, size }), className)} data-slot="badge" {...props} />;
}

export function Kbd({ className, ...props }: React.ComponentProps<"kbd">) {
  return (
    <kbd
      className={cn(
        "pointer-events-none inline-flex h-5 min-w-5 select-none items-center justify-center gap-1 rounded bg-foreground/[0.07] px-1 font-medium font-sans text-muted-foreground text-xs",
        className,
      )}
      {...props}
    />
  );
}

export function Spinner({ className, ...props }: React.ComponentProps<typeof LoaderCircleIcon>) {
  return <LoaderCircleIcon role="status" className={cn("size-4 animate-spin", className)} {...props} />;
}

export function Switch({ className, ...props }: SwitchPrimitive.Root.Props) {
  return (
    <SwitchPrimitive.Root
      className={cn(
        "inline-flex h-[calc(var(--thumb-size)+2px)] w-[calc(var(--thumb-size)*2-2px)] shrink-0 cursor-pointer items-center rounded-full p-[2px] outline-none transition-[background-color,box-shadow] duration-200 [--thumb-size:--spacing(5)] focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background data-checked:bg-primary data-unchecked:bg-input data-disabled:cursor-not-allowed data-disabled:opacity-64 sm:[--thumb-size:--spacing(4)]",
        className,
      )}
      {...props}
    >
      <SwitchPrimitive.Thumb className="pointer-events-none block size-[calc(var(--thumb-size)-2px)] shrink-0 rounded-(--thumb-size) bg-background shadow-sm/5 [transition:translate_.15s] data-checked:translate-x-[calc(var(--thumb-size)-4px)] dark:data-unchecked:bg-muted-foreground" />
    </SwitchPrimitive.Root>
  );
}

const fieldClass =
  "w-full min-w-0 rounded-[var(--control-radius)] border border-input bg-popover not-dark:bg-clip-padding px-[calc(--spacing(3)-1px)] text-base text-foreground shadow-xs/5 outline-none transition-shadow placeholder:text-placeholder focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/24 disabled:opacity-64 dark:bg-input/32 sm:text-sm";

export function Input({ className, ...props }: React.ComponentProps<"input">) {
  return <input className={cn(fieldClass, "h-9 sm:h-8", className)} {...props} />;
}

export function Textarea({ className, ...props }: React.ComponentProps<"textarea">) {
  return <textarea className={cn(fieldClass, "min-h-16 py-1.5 font-mono text-xs sm:text-xs", className)} {...props} />;
}

export function Label({ className, ...props }: React.ComponentProps<"label">) {
  return <label className={cn("font-medium text-sm text-foreground", className)} {...props} />;
}
