import type { ReactNode } from "react";

import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";

export interface SidebarSectionProps {
  /** The uppercase band title (e.g. "Terminals", "Commands"). */
  title: string;
  /**
   * The single hover-revealed action at the band's right edge: the icon glyph, the
   * tooltip/aria label, and the click handler. Optional — a section may have no
   * action. For TERMINALS this is the `+` (new terminal); for COMMANDS the gear
   * (open the manage-commands modal).
   */
  action?: {
    icon: ReactNode;
    /** Tooltip + accessible label for the action button. */
    label: string;
    onClick: () => void;
  };
  /** The section body (the rows list, or an empty-state hint). */
  children: ReactNode;
}

/**
 * `<SidebarSection>` — the SHARED, non-collapsible typed band of the LEGACY sidebar spine
 * (`sidebar/workspace-subsections.tsx`): a quiet uppercase title plus ONE hover-revealed action
 * icon at the right edge, over the section body.
 *
 * ## WHAT FINDING 01KV63TD5E… ASKED FOR, AND WHERE IT NOW LIVES
 *
 * The finding was about ALIGNMENT: TERMINALS was non-collapsible while COMMANDS carried a chevron
 * and a `<CollapsibleSection>`, and one band behaving unlike its neighbour is what it called out.
 * This component was the first answer — align them by taking the fold AWAY, so neither band has
 * one.
 *
 * The bands of the CURRENT tree took the other answer: `terminals/runtime-section.tsx` gives BOTH
 * of them the fold, through one component, with the state remembered per workspace and per band.
 * The requirement is the same and it is met the same way — one shape for both — so nothing here
 * needs changing; what changed is which of the two alignments the product wanted. Read this
 * paragraph before "fixing" either file to match the other: they are not in disagreement, they are
 * two different surfaces, and only one of them is the tree the app renders today.
 *
 * The action reveals on `group-hover/sub` (matching the `group/sub` on the header row), exactly as
 * the old per-band `+` did, so both bands surface their action the same way.
 */
export function SidebarSection({ title, action, children }: SidebarSectionProps) {
  return (
    <div>
      <div className="group/sub flex items-center gap-1 pr-1 pl-1">
        <span className="flex min-w-0 flex-1 items-center gap-1 py-0.5 select-none">
          <span className="text-xs font-semibold tracking-wider text-muted-foreground uppercase">
            {title}
          </span>
        </span>
        {action && (
          <Tooltip label={action.label}>
            <Button
              variant="ghost"
              size="icon-xs"
              aria-label={action.label}
              onClick={action.onClick}
              className="size-5 opacity-0 transition group-hover/sub:opacity-100 focus-visible:opacity-100"
            >
              {action.icon}
            </Button>
          </Tooltip>
        )}
      </div>
      {children}
    </div>
  );
}
