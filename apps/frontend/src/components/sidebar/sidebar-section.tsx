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
 * `<SidebarSection>` — the SHARED, NON-collapsible typed band used by both the
 * TERMINALS and COMMANDS subsections (finding 01KV63TD5E…). Both bands are now the
 * SAME shape: a quiet uppercase title (PLAIN text — there is NO chevron and NO
 * collapse: the title is not a toggle button) plus ONE hover-revealed action icon at
 * the right edge, over the section body.
 *
 * Before this, TERMINALS was non-collapsible but COMMANDS carried a chevron +
 * `<CollapsibleSection>` — the inconsistency the finding called out. Folding both
 * onto this component removes the COMMANDS chevron and gives it a hover action (the
 * gear that opens the manage-commands modal), mirroring TERMINALS' `+`.
 *
 * The action reveals on `group-hover/sub` (matching the `group/sub` on the header
 * row), exactly as the old per-band `+` did, so both bands surface their action the
 * same way.
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
