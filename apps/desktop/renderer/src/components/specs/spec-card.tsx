import { Link } from "@tanstack/react-router";

import { Badge } from "@/components/ui/badge";

import { DependencyBadges } from "./dependency-badges";
import { priorityMeta, specKey } from "./spec-meta";
import { ProgressBar } from "./spec-primitives";
import type { SpecListEntry } from "./types";

/**
 * `<SpecCardView>` — one Spec on the KANBAN board: the human key + priority chip on top, the
 * title, and (when the Spec has tasks) a progress bar with a `done/total` readout. The whole
 * card is a router `<Link>` to the PRD detail (`/work/$specId`) — the same placement the
 * prototype's `spec-card` had, rewritten against the real design system (token colours,
 * the real `<Badge>` primitive, no prototype markup/pixels).
 */
export function SpecCardView({ spec }: { spec: SpecListEntry }) {
  const priority = priorityMeta(spec.priority);
  return (
    <Link
      to="/work/$specId"
      params={{ specId: spec.id }}
      data-testid={`spec-card-${spec.id}`}
      className="block shrink-0 rounded-xl border border-border/70 bg-card p-4 outline-none transition-shadow hover:shadow-md focus-visible:ring-2 focus-visible:ring-ring"
    >
      <div className="flex items-center gap-2">
        <span className="font-mono text-2xs text-muted-foreground">
          {specKey(spec.project_key, spec.number)}
        </span>
        <Badge variant={priority.variant} className="ml-auto">
          {priority.label}
        </Badge>
      </div>
      <h3 className="mt-2 text-sm leading-snug text-foreground">{spec.title}</h3>
      {/* Spec↔Spec dependency badges (PRD "surfacé sur le board") — present only when non-zero. */}
      <DependencyBadges
        blockedBy={spec.blocked_by_count}
        unblocks={spec.unblocks_count}
        className="mt-2"
      />
      {spec.tasks_total > 0 ? (
        <div className="mt-3">
          <ProgressBar done={spec.tasks_done} total={spec.tasks_total} />
          <div className="mt-1 font-mono text-2xs text-muted-foreground">
            {spec.tasks_done} / {spec.tasks_total} tasks
          </div>
        </div>
      ) : null}
    </Link>
  );
}
