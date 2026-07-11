import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

import { MicroLabel } from "./micro-label";

/**
 * `SectionPanel` — a bordered card with a tinted header strip (uppercase mono title + optional
 * trailing action). Ported from the prototype against the v2 tokens; the repeated "section panel"
 * shell (the review tab's per-repo group, …). Tokens only — no proto hex.
 */
export function SectionPanel({
  title,
  action,
  children,
  className,
}: {
  title: ReactNode;
  action?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section
      className={cn("overflow-hidden rounded-xl border border-border/60 bg-card", className)}
    >
      <div className="flex items-center gap-2 border-b border-border/50 bg-muted/40 px-3 py-2.5">
        <MicroLabel>{title}</MicroLabel>
        {action != null ? <div className="ml-auto">{action}</div> : null}
      </div>
      {children}
    </section>
  );
}
