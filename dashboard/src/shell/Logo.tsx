import { cn } from "@/lib/cn";

/** Monochrome mark: three nodes on a ring, a nod to a mesh. */
export function Logo({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" className={cn("text-fg", className)} aria-hidden fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="12" cy="5" r="2.2" fill="currentColor" stroke="none" />
      <circle cx="5.5" cy="17" r="2.2" fill="currentColor" stroke="none" />
      <circle cx="18.5" cy="17" r="2.2" fill="currentColor" stroke="none" />
      <path d="M12 7.2 6.6 15.2M12 7.2l5.4 8M7.7 17h8.6" />
    </svg>
  );
}
