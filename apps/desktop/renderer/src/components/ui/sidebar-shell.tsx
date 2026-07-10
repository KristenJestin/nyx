import type * as React from "react";

import { cn } from "@/lib/utils";

/**
 * `<SidebarShell>` — the SHARED left-hand sidebar CHASSIS used by both full-page rails:
 * the Terminals cockpit (`terminals-page`) and the PRD detail (`spec-detail-page`). It owns
 * only the SHELL — fixed width + LEFT position + right border + full height + the scrollable
 * content region + the surface tokens — matching the terminal sidebar exactly. The CONTENT is
 * injected per page (the terminals page injects its workspace nav; the PRD detail injects its
 * STATUS / MODE / REPOSITORIES / WORKSPACE sections), so the two rails stay one primitive with
 * one geometry and never drift apart again (review finding: two independent sidebars, no shared
 * primitive → extract a common shell, both pages to the LEFT).
 *
 * Layout contract: a `w-[260px] shrink-0` column so it holds its width whether it sits in a
 * flex row (the sibling main uses `flex-1 min-w-0`) or standalone. The optional `header` slot
 * renders in a bordered header block at the top (the terminals page's project title); the
 * `children` fill the SCROLLABLE region below it. Tokens only — no raw hex, no magic px beyond
 * the one shared width.
 */
export function SidebarShell({
  header,
  children,
  className,
  contentClassName,
  "aria-label": ariaLabel,
}: {
  /** Optional pinned header block (bordered, non-scrolling) at the top of the rail. */
  header?: React.ReactNode;
  /** The scrollable rail content (nav / sections). */
  children: React.ReactNode;
  /** Extra classes on the `<aside>` shell (rarely needed — geometry is fixed). */
  className?: string;
  /** Extra classes on the scrollable content region. */
  contentClassName?: string;
  "aria-label"?: string;
}) {
  return (
    <aside
      aria-label={ariaLabel}
      className={cn(
        "flex h-svh w-[260px] shrink-0 flex-col border-r border-border/60 bg-muted/40",
        className,
      )}
    >
      {header != null ? (
        <div className="shrink-0 border-b border-border/60 px-3 py-3">{header}</div>
      ) : null}
      <div className={cn("min-h-0 flex-1 overflow-y-auto", contentClassName)}>{children}</div>
    </aside>
  );
}
