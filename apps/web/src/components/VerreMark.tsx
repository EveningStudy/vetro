import { useId } from "react";
import { cn } from "~/lib/utils";

export function VerreMark({ className }: { className?: string }) {
  const maskId = useId();
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden className={cn("shrink-0", className)}>
      <defs>
        <mask id={maskId}>
          <rect width="24" height="24" fill="white" />
          <circle cx="14.25" cy="10.25" r="6.45" fill="black" />
        </mask>
      </defs>
      <circle cx="9.75" cy="13.75" r="6.1" mask={`url(#${maskId})`} />
      <circle cx="14.25" cy="10.25" r="5.85" fillOpacity="0.52" />
    </svg>
  );
}
