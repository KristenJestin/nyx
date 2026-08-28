import type { ReactNode } from "react";

import { Tooltip } from "@/components/ui/tooltip";
import {
  SidebarLine,
  SidebarLineActions,
  SidebarLineBand,
  SidebarLineGlyph,
  SidebarLineLabel,
} from "@/components/ui/sidebar-line";
import type { RunState } from "@/components/ui/status-dot";
import { useRuntimeSelection, type RuntimeSelection } from "./runtime-selection";

/**
 * `<RuntimeRow>` — the SELECTABLE families of the runtime tree: a terminal, an agent (the same
 * line, wearing its provider's glyph and its activity pastille) and a managed command. Clicking it
 * makes it the shared page selection so the main pane swaps to the matching surface.
 *
 * ## It no longer OWNS a geometry (human review 01M12MW1DV1V455EWBMV048TE6)
 *
 * The band, the glyph size, the insets, the hover tint and the corner pastille are
 * `<SidebarLine>`'s — the one component every family of the tree composes, folders included. This
 * file is now what makes a line SELECTABLE (its band is a `<button>` that selects) and nothing
 * else. That is the point: the folder head's parity with a terminal row is not maintained here or
 * there, it is structural — there is one line, and these are two of its compositions.
 *
 * ## Non-inline hover meta (`tooltip`) — v2.5.6 FEEDBACK #28
 * An optional `tooltip` surfaces per-row meta (a terminal's live CPU%/RAM) ONLY on hover/focus,
 * so the reading NEVER competes with the name for row width (the firm non-inline constraint: v1
 * put stats on the item line and they masked/truncated the name). The name stays fully visible;
 * the meta appears above the row as a tooltip.
 *
 * ## Inline rename (`editing`/`editor`) — v2.5.6
 * When `editing` is set the row swaps its select-button band for `editor` (an `<Input>`), rendered
 * OUTSIDE any `<button>` so the field is a valid, focusable control (interactive content may not
 * nest in a button). `onLabelDoubleClick` lets a double-click on the name begin the rename.
 *
 * Presentation only — the run state is projected upstream (`runtime-state`) and handed in.
 */
export function RuntimeRow({
  selection,
  icon,
  label,
  state,
  actions,
  tooltip,
  editing = false,
  editor,
  onLabelDoubleClick,
}: {
  /** The element this row selects (kind + id), matched against the shared selection. */
  selection: RuntimeSelection;
  /** The lead glyph (a terminal / service / agent-provider icon). */
  icon: ReactNode;
  label: ReactNode;
  /** The row's run-state dot. */
  state: RunState;
  /**
   * Optional trailing actions. WHEN they show is not this file's decision any more — the reveal
   * (hover, selection, focus-visible) belongs to `<SidebarLineActions>`, once, for every family
   * (human review 01M13118SE5M04J4M9SJGZYAAT).
   */
  actions?: ReactNode;
  /**
   * Optional hover/focus tooltip content (v2.5.6, non-inline meta): a terminal's live CPU%/RAM.
   * `undefined` leaves the row untouched (no tooltip wrapper), so a service row is unaffected.
   */
  tooltip?: ReactNode;
  /** When true, render `editor` in place of the label (inline rename mode). */
  editing?: boolean;
  /** The inline editor element (an `<Input>`), rendered outside the select button when `editing`. */
  editor?: ReactNode;
  /** Begin an inline rename on a double-click of the name (mirrors the rename action). */
  onLabelDoubleClick?: () => void;
}) {
  const { select, isSelected } = useRuntimeSelection();
  const selected = isSelected(selection);

  // The lead glyph + its corner run-state pastille — the SHARED one, so an agent's yellow "waiting"
  // dot and a closed folder's rollup are the same object at the same offsets. Built once here so
  // the line keeps a stable left edge between the normal and the renaming branch.
  const glyph = <SidebarLineGlyph icon={icon} state={state} />;

  // The clickable select band (normal, non-editing state). Wrapped in a `<Tooltip>` when `tooltip`
  // is supplied so the hover meta reads WITHOUT ever occupying row width (non-inline).
  const selectBand = (
    <SidebarLineBand
      render={<button type="button" onClick={() => select(selection)} className="text-left" />}
    >
      {glyph}
      <SidebarLineLabel className="text-foreground" onDoubleClick={onLabelDoubleClick}>
        {label}
      </SidebarLineLabel>
    </SidebarLineBand>
  );

  return (
    <SidebarLine
      selected={selected}
      actions={actions != null ? <SidebarLineActions>{actions}</SidebarLineActions> : undefined}
    >
      {editing && editor != null ? (
        <SidebarLineBand>
          {glyph}
          {editor}
        </SidebarLineBand>
      ) : tooltip != null ? (
        <Tooltip label={tooltip} side="top">
          {selectBand}
        </Tooltip>
      ) : (
        selectBand
      )}
    </SidebarLine>
  );
}
