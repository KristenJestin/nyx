import { Link } from "@tanstack/react-router";

import { Badge } from "@/components/ui/badge";
import { ProjectMark } from "@/components/sidebar/project-mark";

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
 *
 * ## THE PROJECT MARK (`showProject`)
 *
 * In the cross-project scope the card OPENS on its project's `<ProjectMark labelled>`, immediately
 * before the human key it completes: the mark is seen, the key is read, and the two say the same
 * thing through two channels. It sits INSIDE the existing key row — the ideas' precedent (the mark
 * rides in the title cell, before the text, never in a lane of its own) applied to this anatomy —
 * so nothing on the card moves and the project scope, where it is simply not rendered, is
 * byte-identical to before.
 *
 * The card used to carry NO mark, deliberately: `BETA-1` names its project in the place the eye
 * already goes. The user looked at the real board and did not recognise it (« je suis en all dans
 * works, j'ai aucun sign du projet en question sur les card ? ») — a key is a string to READ and
 * decode, `DEPOT-8` and `MUSICMAN-28` more than most. The reversal is his call and it stops here:
 * the HOMES' Spec cards (`home/home-spec-card.tsx`) are a different component and keep the elected
 * v11 anatomy, unmarked.
 */
export function SpecCardView({
  spec,
  showProject = false,
}: {
  spec: SpecListEntry;
  /** CROSS-PROJECT scope: open the key row with the Spec's project mark. */
  showProject?: boolean;
}) {
  const priority = priorityMeta(spec.priority);
  return (
    <Link
      to="/work/$specId"
      params={{ specId: spec.id }}
      data-testid={`spec-card-${spec.id}`}
      className="block shrink-0 rounded-xl border border-border/70 bg-card p-4 outline-none transition-shadow hover:shadow-md focus-visible:ring-2 focus-visible:ring-ring"
    >
      <div className="flex items-center gap-2">
        {showProject && spec.project ? (
          <ProjectMark
            project={spec.project}
            labelled
            data-testid="spec-project-mark"
            className="size-5 shrink-0 rounded-md"
          />
        ) : null}
        <span data-testid="spec-key" className="font-mono text-2xs text-muted-foreground">
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
