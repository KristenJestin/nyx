import { motion, type Transition, useReducedMotion } from "motion/react";
import type * as React from "react";

import { cn } from "@/lib/utils";

/**
 * `<SidebarShell>` — the SHARED sidebar CHASSIS. It began as the fixed LEFT rail of both full-page
 * cockpits (the Terminals page + the PRD detail) and now ALSO serves the review screen's two flanks
 * (PDR-0028): a LEFT files rail and a RIGHT, closable comments rail — the SAME primitive placed in
 * mirror.
 *
 * ## Two shapes, one primitive
 * - DEFAULT (`side="left"`, non-collapsible): the original geometry — a `w-[260px] shrink-0`, full-
 *   height column with a RIGHT border and the muted surface. The existing rails (app-sidebar,
 *   terminal, spec) render EXACTLY as before (their tests pin these classes), so nothing drifts.
 * - `side="right"`: the mirror — the border moves to the LEFT edge, geometry otherwise identical.
 * - `collapsible` + `open`: a closable rail whose WIDTH animates between its open width and `0`
 *   (the prototype's `motion` width idiom), driven ENTIRELY by the parent (`open` is controlled).
 *   Reduced-motion collapses instantly.
 *
 * ## A rail that folds to a STUB, not to nothing (`collapsedWidth` + `collapsed`)
 * A closable rail defaults to folding to `0` — it leaves the screen entirely. The prototype page
 * needs the other half of the same idiom (finding `01KYVMSMMBVSZNBBSBE18PRZAK`): its pages rail
 * folds to a 52 px strip of miniatures and its right panel to a 48 px stub, so the scene takes the
 * width back WITHOUT the user losing where they are — and the stub carries the control that unfolds
 * it. `collapsedWidth` is that target width, `collapsed` the content shown at it. Both default to
 * the previous behaviour (`0`, nothing), so the review's two flanks render byte-identically.
 *
 * ## One toggle per flank (Finding 46)
 * A `collapsible` rail carries NO built-in close affordance: it is opened/closed SOLELY by its
 * parent's control (in the review, the two ReviewHeader buttons — the panel-left button for the
 * LEFT files rail, the comment button for the RIGHT comments rail). This removes the earlier
 * asymmetry where the RIGHT rail had a redundant internal "Close panel" arrow (a second control for
 * the same panel) while the LEFT had none — now both flanks behave identically, with the header
 * buttons as the single source of truth.
 *
 * The `header` slot renders in a bordered, non-scrolling block at the top; `children` fill the
 * scrollable region below. Tokens only — no raw hex, no magic px beyond the shared width.
 *
 * ## A FOOT that does not scroll (`footer`)
 * The mirror of `header`, pinned at the BOTTOM with a top border. The prototype rail needs it for its
 * round selector (finding `01KZ46P0FTMJXRTT32H6G5TEEN`): the user wants that control at the foot of
 * the rail, and a foot laid inside `children` rides the scroll of the page list — it is not a foot,
 * it is the end of a list. Optional, so every existing rail renders byte-identically.
 */
export function SidebarShell({
  header,
  footer,
  children,
  className,
  contentClassName,
  "aria-label": ariaLabel,
  side = "left",
  collapsible = false,
  open = true,
  width = 260,
  collapsedWidth = 0,
  collapsed,
}: {
  /** Optional pinned header block (bordered, non-scrolling) at the top of the rail. */
  header?: React.ReactNode;
  /** Optional pinned FOOT block (bordered, non-scrolling) at the bottom of the rail. */
  footer?: React.ReactNode;
  /** The scrollable rail content (nav / sections). */
  children: React.ReactNode;
  /** Extra classes on the shell (rarely needed — geometry is fixed). */
  className?: string;
  /** Extra classes on the scrollable content region. */
  contentClassName?: string;
  "aria-label"?: string;
  /** Which flank the rail sits on: `left` (default, right border) or `right` (mirror, left border). */
  side?: "left" | "right";
  /** When true, the rail's width animates between `width` and `0` on `open` (a closable panel). */
  collapsible?: boolean;
  /** The open state of a `collapsible` rail — controlled ENTIRELY by the parent (Finding 46: the
   *  rail has no internal toggle; its owning page's header button is the single open/close control).
   *  Ignored when not collapsible. */
  open?: boolean;
  /** The rail's open width in px (collapsible mode). Defaults to the shared 260. */
  width?: number;
  /** The width a `collapsible` rail folds DOWN to. `0` (default) = it leaves the screen. */
  collapsedWidth?: number;
  /** What the folded rail shows at `collapsedWidth` (the stub + its unfold control). */
  collapsed?: React.ReactNode;
}) {
  const reduce = useReducedMotion();
  const borderClass = side === "right" ? "border-l" : "border-r";

  const headerBlock =
    header != null ? (
      <div className="shrink-0 border-b border-border/60 px-3 py-3">{header}</div>
    ) : null;
  const footerBlock =
    footer != null ? (
      <div className="shrink-0 border-t border-border/60 px-3 py-3">{footer}</div>
    ) : null;
  const body = (
    <div className={cn("min-h-0 flex-1 overflow-y-auto", contentClassName)}>{children}</div>
  );

  // DEFAULT (non-collapsible): the original fixed-width rail geometry, mirrored by `side`. The
  // existing left rails MUST keep these exact classes (their tests assert them).
  if (!collapsible) {
    return (
      <aside
        aria-label={ariaLabel}
        // The default width lives in the `w-[260px]` class (existing rails pin it). A non-default
        // `width` (e.g. the review's always-open 300px files rail, Finding 58) overrides it inline;
        // the 260 default sets no inline style so those rails render byte-identically to before.
        style={width === 260 ? undefined : { width }}
        className={cn(
          "flex h-svh w-[260px] shrink-0 flex-col bg-muted/40",
          `${borderClass} border-border/60`,
          className,
        )}
      >
        {headerBlock}
        {body}
        {footerBlock}
      </aside>
    );
  }

  // COLLAPSIBLE: the width animates open↔0 (prototype idiom). Kept mounted (aria-hidden when
  // closed) so re-opening animates back rather than remounting. The inner column holds a FIXED
  // width so its content does not reflow while the outer width animates. No internal close control
  // (Finding 46) — the parent's header button is the single open/close affordance for the rail.
  const T: Transition = reduce ? { duration: 0 } : { type: "spring", duration: 0.4, bounce: 0.15 };

  // A folded rail that still SHOWS something (a stub) stays in the a11y tree — it carries the
  // control that unfolds it. One that folds to nothing keeps the previous `aria-hidden`.
  const stubbed = !open && collapsed != null;

  return (
    <motion.aside
      aria-label={ariaLabel}
      aria-hidden={!open && !stubbed}
      data-open={open}
      initial={false}
      animate={{ width: open ? width : collapsedWidth }}
      transition={T}
      className={cn(
        "h-svh shrink-0 overflow-hidden bg-muted/40",
        `${borderClass} border-border/60`,
        className,
      )}
    >
      {stubbed ? (
        <div style={{ width: collapsedWidth }} className="flex h-full flex-col">
          {collapsed}
        </div>
      ) : (
        <div style={{ width }} className="flex h-full flex-col">
          {headerBlock}
          {body}
          {footerBlock}
        </div>
      )}
    </motion.aside>
  );
}
