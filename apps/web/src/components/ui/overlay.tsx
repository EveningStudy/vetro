// Tooltip, Menu, Popover and Dialog styling adapted from T3 Code's components/ui (MIT).
import { Dialog as DialogPrimitive } from "@base-ui/react/dialog";
import { Menu as MenuPrimitive } from "@base-ui/react/menu";
import { Popover as PopoverPrimitive } from "@base-ui/react/popover";
import { Tooltip as TooltipPrimitive } from "@base-ui/react/tooltip";
import { CheckIcon, XIcon } from "lucide-react";
import type * as React from "react";
import { cn } from "~/lib/utils";

export const TooltipProvider = TooltipPrimitive.Provider;

/** A trigger and a short label. `children` is the trigger element. */
export function Tip({
  label,
  side = "top",
  children,
  disabled,
}: {
  label: React.ReactNode;
  side?: TooltipPrimitive.Positioner.Props["side"];
  children: React.ReactElement;
  disabled?: boolean;
}) {
  if (disabled || !label) return children;
  return (
    <TooltipPrimitive.Root>
      <TooltipPrimitive.Trigger render={children} />
      <TooltipPrimitive.Portal>
        <TooltipPrimitive.Positioner side={side} sideOffset={6} className="pointer-events-none z-[140]">
          <TooltipPrimitive.Popup className="relative flex max-w-80 origin-(--transform-origin) rounded-md border bg-popover px-2 py-1 text-popover-foreground text-xs leading-snug shadow-md/5 not-dark:bg-clip-padding transition-[scale,opacity] data-ending-style:scale-98 data-ending-style:opacity-0 data-instant:duration-0 data-starting-style:scale-98 data-starting-style:opacity-0">
            {label}
          </TooltipPrimitive.Popup>
        </TooltipPrimitive.Positioner>
      </TooltipPrimitive.Portal>
    </TooltipPrimitive.Root>
  );
}

export const Menu = MenuPrimitive.Root;
export const MenuTrigger = MenuPrimitive.Trigger;
export const MenuRadioGroup = MenuPrimitive.RadioGroup;

const popupClass =
  "dropdown-glass relative flex origin-(--transform-origin) rounded-lg shadow-[0_16px_40px_-18px_rgb(0_0_0/55%)] outline-none dark:shadow-[0_18px_44px_-18px_rgb(0_0_0/80%)] transition-[scale,opacity] duration-100 data-ending-style:scale-98 data-ending-style:opacity-0 data-starting-style:scale-98 data-starting-style:opacity-0";

export function MenuPopup({
  children,
  className,
  side = "bottom",
  align = "start",
  sideOffset = 4,
}: {
  children: React.ReactNode;
  className?: string;
  side?: MenuPrimitive.Positioner.Props["side"];
  align?: MenuPrimitive.Positioner.Props["align"];
  sideOffset?: number;
}) {
  return (
    <MenuPrimitive.Portal>
      <MenuPrimitive.Positioner side={side} align={align} sideOffset={sideOffset} className="z-[130]">
        <MenuPrimitive.Popup className={cn(popupClass, "min-w-[min(11rem,calc(100vw-2rem))] max-w-[calc(100vw-2rem)]", className)}>
          <div className="max-h-(--available-height) w-full overflow-y-auto p-1">{children}</div>
        </MenuPrimitive.Popup>
      </MenuPrimitive.Positioner>
    </MenuPrimitive.Portal>
  );
}

const itemClass =
  "flex min-h-8 cursor-pointer select-none items-center gap-2 rounded-sm px-2 py-1 text-base text-foreground outline-none data-disabled:pointer-events-none data-disabled:opacity-64 data-highlighted:bg-accent data-highlighted:text-accent-foreground sm:min-h-7 sm:text-sm [&>svg:not([class*='size-'])]:size-4 [&>svg:not([class*='text-'])]:text-muted-foreground [&>svg]:shrink-0";

export function MenuItem({ className, destructive, ...props }: MenuPrimitive.Item.Props & { destructive?: boolean }) {
  return (
    <MenuPrimitive.Item
      className={cn(itemClass, destructive && "text-destructive-foreground [&>svg:not([class*='text-'])]:text-current", className)}
      {...props}
    />
  );
}

export function MenuRadioItem({ className, children, ...props }: MenuPrimitive.RadioItem.Props) {
  return (
    <MenuPrimitive.RadioItem className={cn(itemClass, "pe-7 relative", className)} {...props}>
      {children}
      <MenuPrimitive.RadioItemIndicator className="absolute end-2 flex">
        <CheckIcon className="size-3.5" />
      </MenuPrimitive.RadioItemIndicator>
    </MenuPrimitive.RadioItem>
  );
}

export function MenuCheckboxItem({ className, children, ...props }: MenuPrimitive.CheckboxItem.Props) {
  return (
    <MenuPrimitive.CheckboxItem className={cn(itemClass, "pe-7 relative", className)} {...props}>
      {children}
      <MenuPrimitive.CheckboxItemIndicator className="absolute end-2 flex">
        <CheckIcon className="size-3.5" />
      </MenuPrimitive.CheckboxItemIndicator>
    </MenuPrimitive.CheckboxItem>
  );
}

export function MenuGroupLabel({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("px-2 py-1.5 font-medium text-muted-foreground text-xs", className)} {...props} />;
}

export function MenuSeparator({ className }: { className?: string }) {
  return <MenuPrimitive.Separator className={cn("mx-2 my-1 h-px bg-border", className)} />;
}

export const Popover = PopoverPrimitive.Root;
export const PopoverTrigger = PopoverPrimitive.Trigger;

export function PopoverPopup({
  children,
  className,
  side = "bottom",
  align = "start",
  sideOffset = 6,
  initialFocus,
}: {
  children: React.ReactNode;
  className?: string;
  side?: PopoverPrimitive.Positioner.Props["side"];
  align?: PopoverPrimitive.Positioner.Props["align"];
  sideOffset?: number;
  initialFocus?: PopoverPrimitive.Popup.Props["initialFocus"];
}) {
  return (
    <PopoverPrimitive.Portal>
      <PopoverPrimitive.Positioner side={side} align={align} sideOffset={sideOffset} className="z-[130]">
        <PopoverPrimitive.Popup initialFocus={initialFocus} className={cn(popupClass, "flex-col", className)}>
          {children}
        </PopoverPrimitive.Popup>
      </PopoverPrimitive.Positioner>
    </PopoverPrimitive.Portal>
  );
}

export const Dialog = DialogPrimitive.Root;
export const DialogClose = DialogPrimitive.Close;

export function DialogPopup({
  children,
  className,
  showClose = true,
  position = "center",
}: {
  children: React.ReactNode;
  className?: string;
  showClose?: boolean;
  position?: "center" | "top";
}) {
  return (
    <DialogPrimitive.Portal>
      <DialogPrimitive.Backdrop className="dialog-backdrop fixed inset-0 z-50 transition-all duration-200 data-ending-style:opacity-0 data-starting-style:opacity-0" />
      <div
        className={cn(
          "pointer-events-none fixed inset-0 z-50 grid justify-items-center p-4",
          position === "center" ? "grid-rows-[1fr_auto_1fr]" : "grid-rows-[minmax(0,10vh)_auto_1fr] max-sm:p-2",
        )}
      >
        <DialogPrimitive.Popup
          className={cn(
            "dialog-glass pointer-events-auto relative row-start-2 flex max-h-full min-h-0 w-full max-w-lg min-w-0 flex-col rounded-2xl border text-popover-foreground outline-none transition-[scale,opacity] duration-200 data-ending-style:scale-98 data-ending-style:opacity-0 data-starting-style:scale-98 data-starting-style:opacity-0",
            className,
          )}
        >
          {children}
          {showClose ? (
            <DialogPrimitive.Close className="absolute end-2 top-2 inline-flex size-8 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground sm:size-7">
              <XIcon className="size-4" />
            </DialogPrimitive.Close>
          ) : null}
        </DialogPrimitive.Popup>
      </div>
    </DialogPrimitive.Portal>
  );
}

export function DialogTitle({ className, ...props }: DialogPrimitive.Title.Props) {
  return <DialogPrimitive.Title className={cn("font-semibold text-lg leading-snug", className)} {...props} />;
}

export function DialogDescription({ className, ...props }: DialogPrimitive.Description.Props) {
  return <DialogPrimitive.Description className={cn("text-muted-foreground text-sm", className)} {...props} />;
}
