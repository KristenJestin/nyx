import { useMemo } from "react";
import { LayersIcon } from "lucide-react";

import { StatusDot, type RunState } from "@/components/ui/status-dot";
import { SectionPanel } from "@/components/ui/section-panel";
import type { EngineWorkflowRun } from "@/bridge";

/** One blackboard row (a key + its JSON-stringified value). */
interface BoardRow {
  readonly key: string;
  readonly value: string;
}

/** Parse a `workflow_runs.blackboard` JSON-object string into display rows (a malformed / non-object
 *  blob yields no rows). Scalars render bare; objects/arrays are compact-JSON. */
export function parseBlackboard(blackboard: string): BoardRow[] {
  let parsed: unknown;
  try {
    parsed = JSON.parse(blackboard);
  } catch {
    return [];
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return [];
  return Object.entries(parsed as Record<string, unknown>).map(([key, value]) => ({
    key,
    value: typeof value === "string" ? value : JSON.stringify(value),
  }));
}

/** Project a workflow run status onto the shared {@link RunState} the `StatusDot` paints. An absent
 *  or unknown status is `idle` — a QUIET state the dot hides, since there is nothing to flag until a
 *  run reports (`isQuietState`). Shared with the home cards' last-run indicator (finding #36). */
export function runStateOf(status: string | undefined): RunState {
  switch (status) {
    case "running":
      return "running";
    case "blocked":
      return "waiting";
    case "done":
      return "success";
    case "failed":
      return "error";
    default:
      return "idle";
  }
}

/**
 * The RUNTIME blackboard panel (v2.8 Phase 6). It reads the workflow run's `blackboard` JSON + status
 * + the CURRENT node (the graph highlights the same node) — the "aucun angle mort sur l'exécution"
 * of the annex `engine-graph`. With no run it shows the static "no run" state.
 */
export function BlackboardPanel({
  run,
  currentNodeLabel,
}: {
  run: EngineWorkflowRun | null;
  currentNodeLabel: string | null;
}) {
  const rows = useMemo(() => (run ? parseBlackboard(run.blackboard) : []), [run]);

  return (
    <div className="flex h-full flex-col gap-3 overflow-y-auto p-3">
      <div className="flex items-center gap-2 border-b border-border pb-2">
        <StatusDot state={runStateOf(run?.status)} />
        {/* A FINISHED run is not a runtime. Titling a `done`/`failed` run "RUNTIME" made a closed run
            read as something executing right now (with an empty CURRENT NODE, since a finished run has
            no cursor) — so the title states which of the two it is. */}
        <span className="text-2xs font-semibold uppercase tracking-wide text-foreground">
          {run && (run.status === "running" || run.status === "blocked") ? "Runtime" : "Last run"}
        </span>
        <span className="ml-auto font-mono text-2xs text-muted-foreground">
          {run ? run.status : "no run"}
        </span>
      </div>

      <SectionPanel
        title={
          <span className="inline-flex items-center gap-1.5">
            <LayersIcon className="size-3.5 text-primary" aria-hidden />
            Blackboard
          </span>
        }
      >
        <div className="px-3 py-2">
          {run == null ? (
            <p className="py-2 text-center text-2xs text-muted-foreground">
              No run yet — the graph is shown statically.
            </p>
          ) : (
            <>
              <dl className="flex flex-col">
                <div className="flex items-center gap-2 border-b border-dashed border-border py-1.5">
                  <dt className="w-28 shrink-0 text-2xs uppercase tracking-wide text-muted-foreground">
                    current node
                  </dt>
                  <dd className="min-w-0 flex-1 truncate font-mono text-2xs font-semibold text-primary">
                    {currentNodeLabel ?? "—"}
                  </dd>
                </div>
                {rows.length === 0 ? (
                  <p className="py-2 text-2xs text-muted-foreground">Empty blackboard.</p>
                ) : (
                  rows.map((row) => (
                    <div
                      key={row.key}
                      className="flex items-center gap-2 border-b border-dashed border-border py-1.5 last:border-b-0"
                    >
                      <dt className="w-28 shrink-0 truncate text-2xs uppercase tracking-wide text-muted-foreground">
                        {row.key}
                      </dt>
                      <dd className="min-w-0 flex-1 truncate font-mono text-2xs font-semibold text-foreground">
                        {row.value}
                      </dd>
                    </div>
                  ))
                )}
              </dl>
            </>
          )}
        </div>
      </SectionPanel>
    </div>
  );
}
