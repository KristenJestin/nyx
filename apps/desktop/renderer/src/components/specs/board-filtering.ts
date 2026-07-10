/**
 * Pure board filtering — extracted from the board component so it is testable outside
 * rendering (the prototype's `filtering.ts`, rewritten against the real read model). The
 * board filters Specs by PRIORITY (multi-select facet); a Spec passes when it matches every
 * non-empty facet.
 */
import type { SpecListEntry } from "./types";

export interface BoardFilters {
  /** Selected priorities (canonical `priority` values); empty = no priority filter. */
  readonly priority: readonly string[];
}

/** An empty filter set (nothing selected). */
export const EMPTY_FILTERS: BoardFilters = { priority: [] };

/** A Spec passes when it matches every non-empty facet. */
export function matches(spec: SpecListEntry, filters: BoardFilters): boolean {
  return filters.priority.length === 0 || filters.priority.includes(spec.priority);
}

/** Count the Specs satisfying `pred` (facet-option counts). */
export function tally(
  specs: readonly SpecListEntry[],
  pred: (s: SpecListEntry) => boolean,
): number {
  return specs.filter(pred).length;
}

/** The total number of selected facet values across all facets. */
export function totalSelected(filters: BoardFilters): number {
  return filters.priority.length;
}
