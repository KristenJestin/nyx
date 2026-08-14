import { WorkflowIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { EmptyState } from "@/components/ui/empty-state";
import { Spinner } from "@/components/ui/spinner";
import type { EngineWorkflow } from "@/bridge";

import { useEngineGraph } from "./use-engine";

/**
 * The engine HOME view (finding #36). `/engine` no longer auto-opens a random `ndev` behind a discreet
 * top `<select>`: it opens on a CHOICE — a grid of workflow CARDS. Each card surfaces the workflow's
 * NAME, its SCOPE, its NODE COUNT, its LAST RUN + state (or "no runs yet"), and whether it is a nyx
 * BUILT-IN (`project_id` null) or a PROJECT FORK. Picking a card is the EXPLICIT selection that opens
 * the graph (the caller swaps the view). Read-only — authoring stays MCP-first.
 */
export function EngineHome({
  workflows,
  loading,
  onSelect,
}: {
  workflows: readonly EngineWorkflow[];
  loading: boolean;
  /** The EXPLICIT selection — open the graph for the picked workflow (finding #36). */
  onSelect: (workflowId: string) => void;
}) {
  if (loading) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <Spinner />
      </div>
    );
  }

  if (workflows.length === 0) {
    return (
      <div className="flex flex-1 items-center justify-center">
        <EmptyState className="max-w-md">
          <p className="flex items-center justify-center gap-2 font-medium text-foreground">
            <WorkflowIcon className="size-4 opacity-60" aria-hidden />
            No workflow yet
          </p>
          <p className="mt-1.5">
            The built-in ndev / nspec / ndoc flows are seeded at boot. Enter a project to see its own
            forks here too.
          </p>
        </EmptyState>
      </div>
    );
  }

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <ul
        data-testid="workflow-cards"
        className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3"
      >
        {workflows.map((workflow) => (
          <li key={workflow.id}>
            <WorkflowCard workflow={workflow} onOpen={() => onSelect(workflow.id)} />
          </li>
        ))}
      </ul>
    </div>
  );
}

/**
 * One workflow CARD. It fetches its OWN node count (from the workflow graph) and its LATEST run
 * (newest-first run list) through the SAME read hooks the graph view uses — so a card stays live
 * (an authoring mutation or a `nyx_next` tick re-pulls it) with no bespoke read surface.
 */
function WorkflowCard({
  workflow,
  onOpen,
}: {
  workflow: EngineWorkflow;
  onOpen: () => void;
}) {
  const graphOf = useEngineGraph(workflow.id);
  const graph = graphOf.graph;

  const nodeCount = graph ? graph.nodes.length : null;
  const builtIn = workflow.project_id == null;

  return (
    <button
      type="button"
      data-testid="workflow-card"
      aria-label={`Open ${workflow.name}`}
      onClick={onOpen}
      className={cn(
        "group flex h-full w-full flex-col gap-3 rounded-xl border border-border bg-card p-3.5 text-left outline-none transition-shadow",
        "hover:border-ring/50 hover:shadow-md focus-visible:ring-2 focus-visible:ring-ring",
      )}
    >
      <div className="flex items-start gap-2">
        <span className="flex size-8 shrink-0 items-center justify-center rounded-lg border border-primary/40 bg-primary/10 text-primary">
          <WorkflowIcon className="size-4" aria-hidden />
        </span>
        <h2 className="min-w-0 flex-1 pt-0.5 font-heading text-sm font-semibold text-foreground">
          {workflow.name}
        </h2>
        <Badge
          variant={builtIn ? "info" : "muted"}
          className="shrink-0 uppercase tracking-wide"
        >
          {builtIn ? "built-in" : "project fork"}
        </Badge>
      </div>

      <div className="flex flex-wrap items-center gap-2 text-2xs text-muted-foreground">
        <span className="rounded-md border border-border bg-muted/40 px-1.5 py-0.5 font-mono uppercase tracking-wide">
          {workflow.scope}
        </span>
        <span aria-hidden className="text-muted-foreground/40">
          ·
        </span>
        <span className="font-mono">{nodeCount == null ? "…" : `${nodeCount} nodes`}</span>
      </div>

    </button>
  );
}

