import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

/**
 * The SHARED sidebar row gabarit for both terminal and command items
 * (finding 01KV63TBV7…). Both rows are the SAME shape — extracted here from what
 * was `terminalRowClassName` so a terminal and a command line up identically:
 * same size, same alignment (NO command-specific `pl-5.5` inset), same right-edge
 * actions zone revealed on hover.
 *
 * Structure of a sidebar item (lead → name → actions):
 *  - a LEAD slot: a glyph-with-state-badge (terminal) or a run-state dot (command);
 *  - a NAME slot: the truncating label (+ an optional muted suffix);
 *  - an ACTIONS slot: right-aligned controls revealed at `group-hover`
 *    (the terminal's close `x`; the command's start/stop/relaunch — finding 4).
 *
 * SELECTION is unchanged and stays the shared single-channel model: the row flags
 * itself `aria-current` + `data-rail-row`, and the measured `<ActiveRail>` glides
 * to it (no per-row background fill). The dimmed/active opacity + bold-name model is
 * folded into `sidebarRowClassName`.
 */

/**
 * The className for the VISUAL sidebar row — the clickable element shared by
 * terminal and command items. Generalised from the old `terminalRowClassName` so a
 * command row is the SAME size/alignment as a terminal row.
 *
 * `cursor-pointer`: the WHOLE row is one click target (no dead zone). `select-none`:
 * a click-drag never text-selects the names. Symmetric `px-2`: the row spans the
 * band full-width with no left inset (the old command `pl-5.5` indent was dropped —
 * that was the misalignment the finding called out). `relative` anchors the rail;
 * `overflow-hidden` clips the height-collapse enter/exit.
 */
export function sidebarRowClassName(active: boolean): string {
  return cn(
    "group relative flex cursor-pointer items-center gap-2 overflow-hidden rounded-md px-2 py-1.5 text-left text-sm transition select-none",
    active
      ? "font-medium text-sidebar-foreground opacity-100 hover:bg-sidebar-accent/40"
      : "text-sidebar-foreground/70 opacity-60 hover:bg-sidebar-accent/40 hover:opacity-90",
  );
}

export interface SidebarItemContentProps {
  /** The lead slot: a glyph + state badge (terminal) or a run-state dot (command). */
  lead: ReactNode;
  /** The display name (truncated). */
  name: ReactNode;
  /** An optional muted suffix beside the name (e.g. the terminal's `· zsh`). */
  suffix?: ReactNode;
  /** The right-aligned actions slot (close, or the command lifecycle controls). */
  actions?: ReactNode;
}

/**
 * `<SidebarItemContent>` — the INNER layout shared by terminal + command rows
 * (lead glyph/dot, truncating name + optional suffix, right-aligned actions). It is
 * wrapper-agnostic: the caller supplies the row element (a `motion.li`, a
 * `Reorder.Item`, or a `<button>`) and applies `sidebarRowClassName` to it; this
 * just fills the row's interior so the two item kinds are pixel-identical.
 *
 * The `actions` slot is rendered as-is — its own hover-reveal (`opacity-0` →
 * `group-hover:opacity-100`) lives on the controls, matching the `group` on the row
 * className, so actions appear on hover at the right edge for both item kinds.
 */
export function SidebarItemContent({ lead, name, suffix, actions }: SidebarItemContentProps) {
  return (
    <>
      {lead}
      <span className="flex min-w-0 flex-1 items-baseline gap-1 truncate">
        <span className="min-w-0 truncate">{name}</span>
        {suffix}
      </span>
      {actions}
    </>
  );
}
