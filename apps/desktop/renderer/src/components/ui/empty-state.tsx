import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

/**
 * `EmptyState` — a centered placeholder for empty lists/panels. Ported from the prototype against
 * the v2 tokens. `dashed` (default) = a bordered dashed card; `plain` = a borderless muted line.
 */
export function EmptyState({
  children,
  variant = "dashed",
  className,
}: {
  children: ReactNode;
  variant?: "dashed" | "plain";
  className?: string;
}) {
  return (
    <div
      className={cn(
        "text-center text-xs text-muted-foreground",
        variant === "dashed"
          ? "rounded-lg border border-dashed border-border/50 px-3 py-8 text-muted-foreground/60"
          : "px-2 py-1.5 text-muted-foreground/70",
        className,
      )}
    >
      {children}
    </div>
  );
}
