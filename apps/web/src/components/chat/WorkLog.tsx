// Row layout adapted from T3 Code's components/chat/WorkLog.tsx (MIT).
import { ChevronRightIcon } from "lucide-react";
import { type ReactNode, useId } from "react";
import { cn } from "~/lib/utils";

export function WorkLogRow({
  icon,
  label,
  trailing,
  expanded,
  onToggle,
  children,
  className,
}: {
  icon?: ReactNode;
  label: ReactNode;
  trailing?: ReactNode;
  expanded?: boolean;
  onToggle?: () => void;
  children?: ReactNode;
  className?: string;
}) {
  const detailsId = useId();
  const interactive = onToggle !== undefined;
  return (
    <div className={cn("group/timeline-row relative w-full min-w-0 rounded-md px-0.5 py-0.5 text-left", className)}>
      <div
        role={interactive ? "button" : undefined}
        tabIndex={interactive ? 0 : undefined}
        aria-expanded={interactive ? expanded : undefined}
        aria-controls={interactive ? detailsId : undefined}
        onClick={onToggle}
        onKeyDown={(event) => {
          if (interactive && event.target === event.currentTarget && (event.key === "Enter" || event.key === " ")) {
            event.preventDefault();
            onToggle();
          }
        }}
        className={cn(
          "flex min-h-6 min-w-0 select-none items-center gap-1.5 text-sm leading-relaxed",
          interactive && "cursor-pointer rounded-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring/70",
        )}
      >
        <div className="flex min-w-0 flex-1 items-baseline gap-1.5">
          {icon ? (
            <span className="relative w-6 shrink-0 leading-0">
              <span className="inline-block size-6 overflow-hidden align-[calc(0.5cap_-_0.75rem)]">
                <span className="flex size-full items-center justify-center text-icon-muted">{icon}</span>
              </span>
            </span>
          ) : null}
          <div className="min-w-0 flex-1 truncate text-secondary-label [&_*]:whitespace-nowrap">{label}</div>
        </div>
        {trailing}
        {interactive ? (
          <span className="flex size-4 shrink-0 items-center justify-center" aria-hidden>
            <ChevronRightIcon
              className={cn(
                "size-3 shrink-0 text-icon-muted opacity-70 transition-transform duration-200",
                expanded && "rotate-90",
              )}
            />
          </span>
        ) : null}
      </div>
      {children ? <div id={detailsId}>{children}</div> : null}
    </div>
  );
}

export function WorkLogDetails({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn("ms-7 mt-1 mb-1.5 flex min-w-0 flex-col gap-2 cursor-auto select-text", className)}>{children}</div>;
}
