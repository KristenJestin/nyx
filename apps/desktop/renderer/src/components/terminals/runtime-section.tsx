import type { ReactNode } from "react";
import { ChevronRightIcon } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { CountBadge, EmptyHint, MicroLabel } from "@/components/ui/tree-primitives";
import { CollapsibleSection } from "@/components/sidebar/collapsible-section";
import { itemTransition } from "@/components/sidebar/item-motion";

/**
 * `<RuntimeSection>` — one typed band of a workspace's runtime tree (Terminals, Commands): a dense
 * header (chevron + micro label + trailing count pill + optional actions) over its rows.
 *
 * ## It folds now, and the two bands fold the SAME WAY (finding 01KV63TD5E…)
 *
 * That finding was about ALIGNMENT: Terminals was non-collapsible while Commands carried a chevron,
 * and one band behaving unlike its neighbour is what it called out. The first answer was to remove
 * the chevron from Commands — the alignment held, at the cost of the fold. This is the other
 * answer, and it satisfies the same requirement the other way round: BOTH bands fold, through this
 * one component, so there is no per-band behaviour left to diverge.
 *
 * It is worth having rather than merely permitted. A workspace with a dozen terminals pushes its
 * commands below the fold of the rail; a workspace whose commands are a launcher pressed twice a
 * day costs a screenful of rows to reach the terminals under them. Folding is the only thing that
 * turns either of those into one line.
 *
 * ## Controlled, and the state is the CALLER'S
 *
 * `open` / `onToggle` come from outside, because "which band is folded" is remembered per workspace
 * across launches (`terminals/ui-persistence.ts`) and this component knows nothing about storage —
 * the same division `<CollapsibleSection>` and `useGroupFolds` already draw. Omit `onToggle` and
 * the band is a plain always-open header, with no chevron and no toggle: exactly what it was.
 *
 * The reveal is the SHARED `<CollapsibleSection>` (height + opacity, `sectionVariants`) and the
 * chevron rotates on `itemTransition` — the very motions the workspace accordion above it uses, so
 * a fold at either level is the same gesture. Both honour `prefers-reduced-motion`.
 *
 * Chrome only — the rows are handed in. Tokens / the token-backed scale only, no raw hex.
 */
export function RuntimeSection({
  title,
  count,
  children,
  className,
  actions,
  open = true,
  onToggle,
  toggleLabel,
}: {
  title: ReactNode;
  count: number;
  children: ReactNode;
  className?: string;
  /** Optional trailing action cluster in the header (e.g. the + new terminal, the gear). */
  actions?: ReactNode;
  /** Whether the band's rows are shown. Ignored (always open) when `onToggle` is absent. */
  open?: boolean;
  /**
   * Fold / unfold the band. ABSENT means the band has no fold at all — no chevron, no toggle, no
   * `aria-expanded`. A control that cannot change anything is worse than no control.
   */
  onToggle?: () => void;
  /**
   * The toggle's accessible name. Named explicitly because the visible label is a `<MicroLabel>`
   * ("Terminals") and the header can also carry a `+` or a gear — "the button that says Terminals"
   * would no longer identify one element.
   */
  toggleLabel?: string;
}) {
  const reduced = useReducedMotion();
  const collapsible = onToggle != null;
  const shown = collapsible ? open : true;

  const heading = (
    <>
      {collapsible && (
        <motion.span
          aria-hidden
          animate={{ rotate: shown ? 90 : 0 }}
          transition={itemTransition(reduced)}
          className="flex shrink-0 items-center text-muted-foreground"
        >
          <ChevronRightIcon className="size-3" />
        </motion.span>
      )}
      <MicroLabel>{title}</MicroLabel>
    </>
  );

  return (
    <div className={cn("mt-3 first:mt-0", className)}>
      <div className="flex items-center gap-2 px-3 pb-1">
        {collapsible ? (
          <button
            type="button"
            aria-expanded={shown}
            aria-label={toggleLabel}
            onClick={onToggle}
            className="flex min-w-0 cursor-pointer items-center gap-1 rounded-sm text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            {heading}
          </button>
        ) : (
          heading
        )}
        {/* THE COUNT STAYS VISIBLE WHEN THE BAND IS SHUT — that is the point of folding one: a
            closed band still says how much is inside it, or it would hide what it is hiding. */}
        <CountBadge className="ml-auto">{count}</CountBadge>
        {actions != null ? <span className="flex items-center gap-0.5">{actions}</span> : null}
      </div>
      {collapsible ? <CollapsibleSection open={shown}>{children}</CollapsibleSection> : children}
    </div>
  );
}

/**
 * `<RuntimeSectionList>` — the rows of a section, or the muted empty hint when the section is
 * empty. Rows are laid on a tight vertical rhythm; the empty state reuses `<EmptyHint>`.
 */
export function RuntimeSectionList({ rows, empty }: { rows: readonly ReactNode[]; empty: string }) {
  if (rows.length === 0) {
    return <EmptyHint>{empty}</EmptyHint>;
  }
  return <div className="flex flex-col gap-0.5 px-2">{rows}</div>;
}
