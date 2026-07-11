import { Children, Fragment, type ReactNode } from "react";

import { cn } from "@/lib/utils";

/**
 * `<Breadcrumb>` — the SHARED breadcrumb chassis for the Work surfaces (Finding 47). Before this, the
 * three pages diverged: the board had a bare `<h1>`, the Spec detail a hand-rolled `Work / <key>`
 * nav, and the review a lone square `←` button — no reusable primitive existed (only the unused
 * `prototype/…/breadcrumb.tsx`). This extracts the Spec-detail breadcrumb into one component so all
 * three render IDENTICALLY: board `Work`, Spec detail `Work / <specKey>`, review
 * `Work / <specKey> / Review`.
 *
 * ## Composable by design (keeps the router typing intact)
 * It renders its CHILDREN with `/` separators woven between them — the caller passes real
 * `<Link>`s (styled with {@link breadcrumbLinkClass}) and a {@link BreadcrumbCurrent} for the last,
 * non-link crumb. Keeping the `<Link>`s at the call site preserves TanStack Router's typed
 * `to`/`params` inference (a generic `to: string` wrapper would lose it), while the separators +
 * link/label styling stay shared here. `data-testid="breadcrumb"` anchors the gate.
 */
export function Breadcrumb({ children, className }: { children: ReactNode; className?: string }) {
  const items = Children.toArray(children).filter(Boolean);
  return (
    <nav
      aria-label="Breadcrumb"
      data-testid="breadcrumb"
      className={cn("flex items-center gap-1.5 text-2xs text-muted-foreground", className)}
    >
      {items.map((child, i) => (
        <Fragment key={i}>
          {i > 0 ? (
            <span aria-hidden className="text-muted-foreground/50">
              /
            </span>
          ) : null}
          {child}
        </Fragment>
      ))}
    </nav>
  );
}

/** The shared LINK crumb styling — apply to a `<Link>` inside a `<Breadcrumb>` (hover + focus ring). */
export const breadcrumbLinkClass =
  "rounded-sm outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring";

/**
 * The CURRENT (last) crumb — a non-link, `aria-current="page"` label. `mono` renders it in the
 * mono font (used for a Spec's human key, e.g. `NYX-3`).
 */
export function BreadcrumbCurrent({
  children,
  mono,
  className,
}: {
  children: ReactNode;
  mono?: boolean;
  className?: string;
}) {
  return (
    <span aria-current="page" className={cn("text-foreground/80", mono && "font-mono", className)}>
      {children}
    </span>
  );
}
