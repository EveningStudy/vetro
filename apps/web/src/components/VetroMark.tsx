import { cn } from "~/lib/utils";

export function VetroMark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" aria-hidden className={cn("shrink-0", className)}>
      <rect x="3" y="3" width="18" height="18" rx="5" className="fill-foreground" />
      <path d="M7.5 8.5 12 16l4.5-7.5" fill="none" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" className="stroke-background" />
    </svg>
  );
}
