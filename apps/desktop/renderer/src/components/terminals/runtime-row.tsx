import type { ReactNode } from "react";

import { cn } from "@/lib/utils";
import { Tooltip } from "@/components/ui/tooltip";
import { StatusDot, type RunState } from "./status-dot";
import { useRuntimeSelection, type RuntimeSelection } from "./runtime-selection";

/**
 * `<RuntimeRow>` — one selectable element of the runtime tree (a terminal or a service). The
 * lead glyph carries a corner run-state dot, then the label, then an optional trailing action
 * cluster (a service's start/stop/relaunch, a terminal's rename/close). Clicking the row makes
 * it the shared page selection so the main pane swaps to the matching surface; the row highlights
 * when selected.
 *
 * ## Non-inline hover meta (`tooltip`) — v2.5.6 FEEDBACK #28
 * An optional `tooltip` surfaces per-row meta (a terminal's live CPU%/RAM) ONLY on hover/focus,
 * so the reading NEVER competes with the name for row width (the firm non-inline constraint: v1
 * put stats on the item line and they masked/truncated the name). The name stays fully visible;
 * the meta appears above the row as a tooltip.
 *
 * ## Inline rename (`editing`/`editor`) — v2.5.6
 * When `editing` is set the row swaps its select-button label for `editor` (an `<Input>`), rendered
 * OUTSIDE any `<button>` so the field is a valid, focusable control (interactive content may not
 * nest in a button). `onLabelDoubleClick` lets a double-click on the name begin the rename.
 *
 * Presentation only — the run state is projected upstream (`runtime-state`) and handed in.
 * Every colour/size is a design-system token (`bg-muted`, `text-foreground`, `ring-background`,
 * the `size-*` scale): no raw hex, no magic px.
 */
export function RuntimeRow({
  selection,
  icon,
  label,
  state,
  badge,
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
   * An optional lifecycle badge shown INLINE after the label (a `<LifecycleBadge>`) — the SEPARATE
   * post-crash doubt axis (`interrupted` / `unknown`), distinct from the run-state dot. `null` in
   * the common (no-crash) case, so a healthy row stays name + dot only.
   */
  badge?: ReactNode;
  /** Optional trailing actions (revealed on hover / while selected). */
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

  // The lead glyph + its corner run-state dot, shared by the normal select-button branch and the
  // editing branch (so the row keeps a stable left edge while renaming).
  const glyph = (
    <span className="relative inline-flex shrink-0 text-muted-foreground">
      {icon}
      <StatusDot
        state={state}
        size="xs"
        className="-right-0.5 -bottom-0.5 absolute rounded-full ring-2 ring-background"
      />
    </span>
  );

  // The clickable select button (normal, non-editing state). Wrapped in a `<Tooltip>` when
  // `tooltip` is supplied so the hover meta reads WITHOUT ever occupying row width (non-inline).
  const selectButton = (
    <button
      type="button"
      onClick={() => select(selection)}
      className="flex min-w-0 flex-1 items-center gap-2.5 px-2 py-1.5 text-left"
    >
      {glyph}
      <span className="min-w-0 flex-1 truncate text-foreground" onDoubleClick={onLabelDoubleClick}>
        {label}
      </span>
      {badge != null ? <span className="shrink-0">{badge}</span> : null}
    </button>
  );

  return (
    <div
      data-selected={selected}
      className={cn(
        "group flex items-center gap-1 rounded-md pr-1 text-sm transition-colors",
        selected ? "bg-muted" : "hover:bg-muted/60",
      )}
    >
      {editing && editor != null ? (
        <div className="flex min-w-0 flex-1 items-center gap-2.5 px-2 py-1.5">
          {glyph}
          {editor}
        </div>
      ) : tooltip != null ? (
        <Tooltip label={tooltip} side="top">
          {selectButton}
        </Tooltip>
      ) : (
        selectButton
      )}
      {actions != null ? (
        <span className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100 group-data-[selected=true]:opacity-100">
          {actions}
        </span>
      ) : null}
    </div>
  );
}
