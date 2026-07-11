import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

/**
 * `MicroLabel` — the dense uppercase mono section/meta label. Ported from the prototype against
 * the v2 design tokens (`font-mono text-2xs uppercase tracking-wide text-muted-foreground`), the
 * single source for the small section headers (review "Comments by repo", side-panel groups…).
 */
export function MicroLabel({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <span
      className={cn("font-mono text-2xs uppercase tracking-wide text-muted-foreground", className)}
    >
      {children}
    </span>
  );
}
