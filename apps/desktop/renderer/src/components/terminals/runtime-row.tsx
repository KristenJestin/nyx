import type { ReactNode } from "react";

import { cn } from "@/lib/utils";
import { StatusDot, type RunState } from "./status-dot";
import { useRuntimeSelection, type RuntimeSelection } from "./runtime-selection";

/**
 * `<RuntimeRow>` — one selectable element of the runtime tree (a terminal or a service). The
 * lead glyph carries a corner run-state dot, then the label, then an optional trailing action
 * cluster (a service's start/stop/relaunch). Clicking the row makes it the shared page
 * selection so the main pane swaps to the matching surface; the row highlights when selected.
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
}) {
  const { select, isSelected } = useRuntimeSelection();
  const selected = isSelected(selection);

  return (
    <div
      data-selected={selected}
      className={cn(
        "group flex items-center gap-1 rounded-md pr-1 text-sm transition-colors",
        selected ? "bg-muted" : "hover:bg-muted/60",
      )}
    >
      <button
        type="button"
        onClick={() => select(selection)}
        className="flex min-w-0 flex-1 items-center gap-2.5 px-2 py-1.5 text-left"
      >
        <span className="relative inline-flex shrink-0 text-muted-foreground">
          {icon}
          <StatusDot
            state={state}
            size="xs"
            className="-right-0.5 -bottom-0.5 absolute rounded-full ring-2 ring-background"
          />
        </span>
        <span className="min-w-0 flex-1 truncate text-foreground">{label}</span>
        {badge != null ? <span className="shrink-0">{badge}</span> : null}
      </button>
      {actions != null ? (
        <span className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100 group-data-[selected=true]:opacity-100">
          {actions}
        </span>
      ) : null}
    </div>
  );
}
