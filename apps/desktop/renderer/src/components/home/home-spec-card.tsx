import { Link } from "@tanstack/react-router";

import { Badge } from "@/components/ui/badge";
import { ProgressBar } from "@/components/specs/spec-primitives";
import { statusMeta } from "@/components/specs/spec-meta";

import { formatAge } from "./home-format";
import type { HomeSpec } from "./types";

/**
 * `<HomeSpecCard>` — one recently-touched Spec, as the home shows it.
 *
 * Three lines, in this order and no other (elected rounds v10 → v11):
 *
 *   1. the human KEY on the left, the status chip + the age on the right;
 *   2. the TITLE;
 *   3. the ADVANCEMENT, on a fixed three-track grid `[bar][counter][phase]`:
 *      · the BAR opens the line, at the same left edge as the key and the title above it, so it
 *        never floats in from the middle of the card;
 *      · the COUNTER follows IMMEDIATELY, in a reserved track, left-aligned with tabular figures
 *        so every counter on the page starts at the same x whatever its width — it is the bar
 *        said in figures, and it belongs beside the bar, not against the card's right edge;
 *      · the PHASE takes the rest and ellipsises. A Spec with NO phase plan simply ends on its
 *        counter: the empty track is the LAST one, the one nobody sees.
 *      A Spec with no tasks draws no third line at all.
 *
 * The card carries NO project mark, on either home: its key already names the project
 * (`NYX-42`), and a mark would be the same fact twice. The whole card is a router `<Link>` to the
 * Spec detail — the same target the board's card uses.
 */
export function HomeSpecCard({ spec, now }: { spec: HomeSpec; now: number }) {
  const status = statusMeta(spec.status);
  return (
    <Link
      to="/work/$specId"
      params={{ specId: spec.spec_id }}
      data-testid={`home-spec-${spec.spec_id}`}
      className="block cursor-pointer rounded-lg px-2.5 py-2 outline-none transition-colors hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
    >
      <span className="flex items-center gap-2">
        <span className="font-mono text-2xs text-muted-foreground">{spec.key}</span>
        <Badge variant={status.variant} className="ml-auto">
          {status.label}
        </Badge>
        <span className="font-mono text-2xs text-muted-foreground">
          {formatAge(spec.updated_at, now)}
        </span>
      </span>
      <span className="mt-1 block truncate text-xs text-foreground">{spec.title}</span>
      {spec.tasks_total > 0 ? (
        <span
          data-testid="home-spec-progress"
          className="mt-1.5 grid grid-cols-[9.375rem_2.625rem_1fr] items-center gap-x-2.5"
        >
          <ProgressBar done={spec.tasks_done} total={spec.tasks_total} className="col-start-1" />
          <span className="col-start-2 font-mono text-2xs tabular-nums text-muted-foreground">
            {spec.tasks_done}/{spec.tasks_total}
          </span>
          {spec.phase ? (
            <span className="col-start-3 truncate text-2xs text-muted-foreground">
              phase {spec.phase.number}/{spec.phase.total}
              {spec.phase.name ? ` · ${spec.phase.name}` : ""}
            </span>
          ) : null}
        </span>
      ) : null}
    </Link>
  );
}
