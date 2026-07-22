import { cn } from "@/lib/utils";
import { Dot } from "@/components/specs/spec-primitives";

import { IDEA_STATUS_META, IDEA_STATUS_ORDER } from "./idea-meta";

/** The per-status counts a group header distributes (mirror of `IdeaThemeGroup`'s buckets). */
export interface IdeaStatusCounts {
  readonly open: number;
  readonly planned: number;
  readonly implemented: number;
  readonly closed: number;
  readonly dropped: number;
}

/**
 * `<IdeaStatusDots>` — the mini status DISTRIBUTION shown on a theme group header (PRD "mini-dots de
 * répartition statut") and reusable anywhere a compact status breakdown is wanted. One `<dot> n` per
 * NON-ZERO status bucket, in the canonical order. Reuses the shared `<Dot>` primitive + the idea
 * status tokens — no raw colour.
 */
export function IdeaStatusDots({
  counts,
  className,
}: {
  counts: IdeaStatusCounts;
  className?: string;
}) {
  const buckets = IDEA_STATUS_ORDER.filter((status) => counts[status] > 0);
  if (buckets.length === 0) return null;
  return (
    <div className={cn("flex items-center gap-2.5", className)}>
      {buckets.map((status) => {
        const meta = IDEA_STATUS_META[status];
        return (
          <span
            key={status}
            title={meta.label}
            className="inline-flex items-center gap-1 font-mono text-2xs text-muted-foreground"
          >
            <Dot tone={meta.dot} size="xs" />
            {counts[status]}
          </span>
        );
      })}
    </div>
  );
}
