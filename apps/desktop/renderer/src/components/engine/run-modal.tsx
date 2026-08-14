import { Fragment, useCallback, useMemo, useState } from "react";
import { WorkflowIcon, XIcon } from "lucide-react";

import type { EngineWorkflowRun } from "@/bridge";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Dialog, DialogBackdrop, DialogPopup } from "@/components/ui/dialog";
import { EmptyState } from "@/components/ui/empty-state";
import { Spinner } from "@/components/ui/spinner";

import { useSpecRuns } from "./use-engine";
import { WorkflowGraphView } from "./workflow-graph-view";

/**
 * `<SpecRunModal>` — the Spec's ORCHESTRATION RUN, in a modal. This is where the runtime lives now:
 * a run belongs to a Spec, so it is opened from the Spec (its detail page and the dev overlay), not
 * from `/engine` — which describes STRUCTURE and would otherwise surface a long-finished run as if
 * something were executing.
 *
 * It reads the Spec's runs (`engine_spec_runs`, newest-first across every workflow) and shows the
 * NEWEST one: `started_at DESC` puts the most recent activity first, which for a Spec being built is
 * the child workflow ndev delegated to. The graph is the SHARED {@link WorkflowGraphView}, given that
 * run — so the cursor sits on the node the run is parked on and the blackboard panel follows the
 * run's variables. No run yet ⇒ an explicit empty state (nothing has orchestrated this Spec).
 */
export function SpecRunModal({
  specId,
  open,
  onClose,
}: {
  specId: string;
  open: boolean;
  onClose: () => void;
}) {
  // `null` while closed ⇒ the hook stays idle and the graph is torn down with the popup.
  const { runs, latest, loading } = useSpecRuns(open ? specId : null);
  // The run ON SCREEN. It DEFAULTS to the root run (the flow the human launched); the sub-runs its
  // graph delegated to are reachable below but never open by themselves — landing on the coder
  // sub-flow when you asked for /ndev is disorienting, since it is not the flow you started.
  const [shownId, setShownId] = useState<string | null>(null);
  const shown = runs.find((r) => r.id === shownId) ?? latest;
  const children = shown ? runs.filter((r) => r.parent_run_id === shown.id) : [];
  // The ancestor chain of the run on screen, root FIRST — walked through `parent_run_id`, which is
  // exactly what makes the hierarchy queryable in both directions.
  const trail = useMemo(() => {
    const chain: EngineWorkflowRun[] = [];
    let cur: EngineWorkflowRun | undefined = shown ?? undefined;
    const seen = new Set<string>();
    while (cur && !seen.has(cur.id)) {
      seen.add(cur.id);
      chain.unshift(cur);
      const parentId: string | null = cur.parent_run_id;
      cur = parentId ? runs.find((r) => r.id === parentId) : undefined;
    }
    return chain;
  }, [shown, runs]);

  // Each run's workflow NAME, learned as its graph loads (the trail labels itself with them).
  const [names, setNames] = useState<Record<string, string>>({});
  const workflowName = shown ? names[shown.id] : undefined;
  const handleName = useCallback(
    (n: string | null) => {
      if (!n || !shown) return;
      setNames((prev) => (prev[shown.id] === n ? prev : { ...prev, [shown.id]: n }));
    },
    [shown],
  );

  // Clicking a `sub_workflow` node descends into the child run it opened. With several children (a
  // per-phase coder), the ONE that node opened is not distinguishable from the node id alone, so the
  // newest child is taken — the pass being worked on.
  const enterSubWorkflow = useCallback(() => {
    const target = children[children.length - 1] ?? children[0];
    if (target) setShownId(target.id);
  }, [children]);

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
    >
      <Dialog.Portal>
        <DialogBackdrop />
        <DialogPopup className="flex h-[min(46rem,calc(100vh-4rem))] w-[min(78rem,calc(100vw-3rem))] flex-col overflow-hidden p-0">
          <div className="flex shrink-0 items-center gap-2.5 border-b border-border px-5 py-3.5">
            <WorkflowIcon className="size-4 shrink-0 text-primary" aria-hidden />
            <Dialog.Title className="text-sm font-semibold">Orchestration run</Dialog.Title>
            {/* The WORKFLOW being watched — the one thing the header could not otherwise tell you. The
                run's status is NOT repeated here: the runtime panel states it right below. */}
            {trail.length <= 1 && workflowName ? (
              <span className="truncate font-mono text-2xs text-muted-foreground">
                {workflowName}
              </span>
            ) : null}
            {/* The run TRAIL — where you are in the run hierarchy, and every ancestor clickable to go
                back up. A sub-run is reached by clicking its `sub_workflow` node in the graph (or a
                chip below), never by opening on it: the flow you launched is the one you asked for. */}
            {trail.length > 1 ? (
              <nav aria-label="Run hierarchy" className="flex min-w-0 items-center gap-1">
                {trail.map((r, i) => {
                  const last = i === trail.length - 1;
                  return (
                    <Fragment key={r.id}>
                      {i > 0 ? (
                        <span aria-hidden className="shrink-0 text-muted-foreground/50">
                          ›
                        </span>
                      ) : null}
                      <button
                        type="button"
                        disabled={last}
                        onClick={() => setShownId(r.id)}
                        className={cn(
                          "max-w-[12rem] shrink-0 truncate font-mono text-2xs transition-colors",
                          last
                            ? "font-semibold text-foreground"
                            : "text-muted-foreground hover:text-foreground",
                        )}
                      >
                        {names[r.id] ?? "…"}
                      </button>
                    </Fragment>
                  );
                })}
              </nav>
            ) : null}
            {children.map((c, i) => (
              <button
                key={c.id}
                type="button"
                onClick={() => setShownId(c.id)}
                className="shrink-0 rounded border border-border px-1.5 py-0.5 font-mono text-2xs text-muted-foreground transition-colors hover:text-foreground"
              >
                sub-run {children.length > 1 ? i + 1 : ""} · {c.status}
              </button>
            ))}
            {/* An icon close, like the node inspector's — a labelled "Close" button read as a primary
                action next to the title instead of the dismissal it is. */}
            <Dialog.Close
              render={
                <button
                  type="button"
                  aria-label="Close"
                  className="ml-auto rounded-md p-1 text-muted-foreground outline-none transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <XIcon className="size-4" />
                </button>
              }
              onClick={onClose}
            />
          </div>

          <div className="flex min-h-0 flex-1 flex-col" data-testid="spec-run-modal-body">
            {loading ? (
              <div className="flex flex-1 items-center justify-center">
                <Spinner />
              </div>
            ) : shown == null ? (
              <div className="flex flex-1 items-center justify-center">
                <EmptyState className="max-w-md">
                  <p className="flex items-center justify-center gap-2 font-medium text-foreground">
                    <WorkflowIcon className="size-4 opacity-60" aria-hidden />
                    No run for this Spec yet
                  </p>
                  <p className="mt-1.5">
                    Nothing has orchestrated this Spec — a run appears here once /ndev (or another
                    flow) starts on it.
                  </p>
                </EmptyState>
              </div>
            ) : (
              <WorkflowGraphView
                key={shown.id}
                workflowId={shown.workflow_id}
                run={shown}
                onWorkflowName={handleName}
                onEnterSubWorkflow={enterSubWorkflow}
                enterSubWorkflowLabel="Open the child run" 
                showRuntimePanel
              />
            )}
          </div>
        </DialogPopup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

/**
 * `<SpecRunButton>` — the entry point onto {@link SpecRunModal}, owning its own open state so both
 * call sites (the Spec detail header and the dev/creation overlay headers) are a single element.
 * `onClick` stops propagation: the overlay headers carry a toggle/close handler that must NEVER fire
 * from an action.
 */
export function SpecRunButton({ specId, className }: { specId: string; className?: string }) {
  const [open, setOpen] = useState(false);

  return (
    <>
      <Button
        variant="outline"
        size="xs"
        className={className}
        aria-label="Open the orchestration run"
        onClick={(e) => {
          e.stopPropagation();
          setOpen(true);
        }}
      >
        <WorkflowIcon />
        Run
      </Button>
      <SpecRunModal specId={specId} open={open} onClose={() => setOpen(false)} />
    </>
  );
}
