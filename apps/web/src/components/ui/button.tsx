// Adapted from T3 Code's components/ui/button.tsx (MIT).
import { mergeProps } from "@base-ui/react/merge-props";
import { useRender } from "@base-ui/react/use-render";
import { cva, type VariantProps } from "class-variance-authority";
import type * as React from "react";
import { cn } from "~/lib/utils";

const buttonVariants = cva(
  "[--control-icon-color:currentColor] [&_svg]:-mx-0.5 relative inline-flex shrink-0 cursor-pointer items-center justify-center gap-2 whitespace-nowrap rounded-[var(--control-radius)] border font-medium text-base outline-none transition-[box-shadow,scale,background-color,color] [&:active:not([aria-haspopup])]:scale-[0.97] before:pointer-events-none before:absolute before:inset-0 before:rounded-[calc(var(--control-radius)-1px)] pointer-coarse:after:absolute pointer-coarse:after:size-full pointer-coarse:after:min-h-11 pointer-coarse:after:min-w-11 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-64 aria-disabled:cursor-not-allowed aria-disabled:opacity-64 sm:text-sm [&_svg:not([class*='text-'])]:text-[var(--control-icon-color)] [&_svg:not([class*='size-'])]:size-4.5 sm:[&_svg:not([class*='size-'])]:size-4 [&_svg]:pointer-events-none [&_svg]:shrink-0",
  {
    defaultVariants: { size: "default", variant: "default" },
    variants: {
      size: {
        default: "h-9 px-[calc(--spacing(3)-1px)] sm:h-8",
        sm: "h-8 gap-1.5 px-[calc(--spacing(2.5)-1px)] sm:h-7",
        xs: "h-7 gap-1 px-[calc(--spacing(2)-1px)] text-sm sm:h-6 sm:text-xs [&_svg:not([class*='size-'])]:size-4 sm:[&_svg:not([class*='size-'])]:size-3.5",
        lg: "h-10 px-[calc(--spacing(3.5)-1px)] sm:h-9",
        icon: "size-9 sm:size-8",
        "icon-sm": "size-8 sm:size-7",
        "icon-xs":
          "size-7 sm:size-6 [&_svg:not([class*='size-'])]:size-4 sm:[&_svg:not([class*='size-'])]:size-3.5",
      },
      variant: {
        default:
          "not-disabled:inset-shadow-[0_1px_--theme(--color-white/16%)] border-primary bg-primary text-primary-foreground shadow-primary/24 shadow-xs [:active,[data-pressed]]:inset-shadow-[0_1px_--theme(--color-black/8%)] [:disabled,:active,[data-pressed]]:shadow-none [:hover,[data-pressed]]:bg-primary/90",
        destructive:
          "not-disabled:inset-shadow-[0_1px_--theme(--color-white/16%)] border-destructive bg-destructive text-white shadow-destructive/24 shadow-xs [:disabled,:active,[data-pressed]]:shadow-none [:hover,[data-pressed]]:bg-destructive/90",
        "destructive-outline":
          "border-input bg-popover not-dark:bg-clip-padding text-destructive-foreground shadow-xs/5 dark:bg-input/32 [:disabled,:active,[data-pressed]]:shadow-none [:hover,[data-pressed]]:border-destructive/32 [:hover,[data-pressed]]:bg-destructive/4",
        ghost:
          "[--control-icon-color:var(--muted-foreground)] border-transparent text-foreground data-pressed:bg-accent [:hover,[data-pressed]]:bg-accent",
        "ghost-muted":
          "border-transparent text-muted-foreground data-pressed:bg-accent [:hover,[data-pressed]]:bg-accent [:hover,[data-pressed]]:text-foreground",
        outline:
          "[--control-icon-color:var(--muted-foreground)] border-input bg-popover not-dark:bg-clip-padding text-foreground shadow-xs/5 not-disabled:not-active:not-data-pressed:before:shadow-[0_1px_--theme(--color-black/4%)] dark:bg-input/32 dark:not-disabled:not-active:not-data-pressed:before:shadow-[0_-1px_--theme(--color-white/6%)] [:disabled,:active,[data-pressed]]:shadow-none [:hover,[data-pressed]]:bg-accent/50 dark:[:hover,[data-pressed]]:bg-input/64",
        secondary:
          "border-transparent bg-secondary text-secondary-foreground [:hover,[data-pressed]]:bg-accent",
        "warning-outline":
          "border-warning/32 bg-warning-surface text-warning-foreground shadow-xs/5 [:hover,[data-pressed]]:border-warning/40 [:hover,[data-pressed]]:bg-warning/16",
        link: "border-transparent underline-offset-4 [:hover,[data-pressed]]:underline",
      },
    },
  },
);

type ButtonVariant = NonNullable<VariantProps<typeof buttonVariants>["variant"]>;
type ButtonSize = NonNullable<VariantProps<typeof buttonVariants>["size"]>;

interface ButtonProps extends useRender.ComponentProps<"button"> {
  variant?: ButtonVariant;
  size?: ButtonSize;
}

function Button({ className, variant, size, render, ...props }: ButtonProps) {
  const type: React.ButtonHTMLAttributes<HTMLButtonElement>["type"] = render ? undefined : "button";
  return useRender({
    defaultTagName: "button",
    props: mergeProps<"button">(
      { className: cn(buttonVariants({ className, size, variant })), type, "data-slot": "button" } as never,
      props,
    ),
    render,
  });
}

export { Button, buttonVariants, type ButtonSize, type ButtonVariant };
