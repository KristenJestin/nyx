import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

/**
 * Small presentational primitives shared across the Terminals tree — the section micro
 * label, the count pill, the empty hint. Factored out so every section renders identically.
 * Every value is a design-system token or the token-backed Tailwind scale: no raw hex, no
 * magic px. `text-2xs` is the shared `--text-2xs` token from globals.css.
 */

/** A dense, uppercase micro label — the section headers ("Terminals", "Commands"). */
export function MicroLabel({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <span
      className={cn(
        "font-medium text-2xs uppercase tracking-wide text-muted-foreground",
        className,
      )}
    >
      {children}
    </span>
  );
}

/** A tiny count pill sitting at the end of a section header. */
export function CountBadge({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <span
      className={cn(
        "inline-flex min-w-4 items-center justify-center rounded-full bg-muted px-1 text-2xs text-muted-foreground",
        className,
      )}
    >
      {children}
    </span>
  );
}

/** The italic, muted "nothing here" hint an empty section renders in place of rows. */
export function EmptyHint({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <p className={cn("px-2 py-1.5 text-left text-sm italic text-muted-foreground/70", className)}>
      {children}
    </p>
  );
}
