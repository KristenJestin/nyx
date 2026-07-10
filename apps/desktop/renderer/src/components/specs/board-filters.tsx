import { XIcon } from "lucide-react";

import { cn } from "@/lib/utils";

import { PRIORITY_META, PRIORITY_ORDER } from "./spec-meta";
import { Dot } from "./spec-primitives";
import { tally, totalSelected, type BoardFilters } from "./board-filtering";
import type { SpecListEntry, SpecPriority } from "./types";

/**
 * `<BoardFilters>` — the board's filter row: a PRIORITY multi-select facet rendered as
 * toggle chips (each with its dot + label + live count), a summary of how many Specs are
 * visible, and a reset control when any filter is active. Placement reproduced from the
 * prototype's `filtering`/`filters` (facet + count + reset), built with design-system
 * tokens and the app's chip idiom (no dropdown dependency, no prototype markup).
 */
export function BoardFilters({
  allSpecs,
  filters,
  onChange,
  visibleCount,
}: {
  /** The unfiltered Specs, so each facet option shows its total count. */
  allSpecs: readonly SpecListEntry[];
  filters: BoardFilters;
  onChange: (next: BoardFilters) => void;
  /** How many Specs pass the current filters (the summary). */
  visibleCount: number;
}) {
  const selectedCount = totalSelected(filters);

  const togglePriority = (value: SpecPriority) => {
    const set = new Set(filters.priority);
    if (set.has(value)) set.delete(value);
    else set.add(value);
    onChange({ ...filters, priority: [...set] });
  };

  const reset = () => onChange({ ...filters, priority: [] });

  return (
    <div className="flex flex-wrap items-center gap-2">
      <span className="text-2xs uppercase tracking-wide text-muted-foreground">Priority</span>
      {PRIORITY_ORDER.map((pr) => {
        const meta = PRIORITY_META[pr];
        const active = filters.priority.includes(pr);
        const count = tally(allSpecs, (s) => s.priority === pr);
        return (
          <button
            key={pr}
            type="button"
            onClick={() => togglePriority(pr)}
            aria-pressed={active}
            className={cn(
              "inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-xs transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring",
              active
                ? "border-primary/60 bg-primary/10 text-foreground"
                : "border-border text-muted-foreground hover:bg-muted/60 hover:text-foreground",
            )}
          >
            <Dot tone={meta.tone} size="xs" />
            {meta.label}
            <span className="font-mono text-2xs text-muted-foreground/70">{count}</span>
          </button>
        );
      })}

      <span className="ml-auto text-2xs text-muted-foreground">
        {visibleCount} of {allSpecs.length} specs
      </span>
      {selectedCount > 0 ? (
        <button
          type="button"
          onClick={reset}
          className="inline-flex items-center gap-1 rounded-full border border-border px-2 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground"
        >
          <XIcon className="size-3" />
          Reset
        </button>
      ) : null}
    </div>
  );
}
