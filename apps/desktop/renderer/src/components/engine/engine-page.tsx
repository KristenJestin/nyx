import { useCallback, useMemo, useState } from "react";
import { WorkflowIcon } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Breadcrumb, BreadcrumbCurrent, breadcrumbLinkClass } from "@/components/ui/breadcrumb";
import { useActiveProject } from "@/components/terminals/active-project";

import { EngineHome } from "./engine-home";
import { EngineWorkflowView } from "./engine-workflow-view";
import { useEngineWorkflows } from "./use-engine";

/**
 * `<EnginePage>` — the routed ENGINE surface (`/engine`): the READ-ONLY React Flow view of the
 * orchestration behavior trees (v2.8 Phase 6). It opens on a CHOICE (finding #36) — a grid of workflow
 * CARDS (the built-in ndev/nspec/ndoc defaults + the entered project's own forks) — and only opens the
 * graph on an EXPLICIT selection; the breadcrumb's "Engine" crumb walks back to the cards. The graph
 * itself (custom nodes per type, loop/sequence group containers, ELK compound layout — finding #34)
 * and the TYPE-ADAPTED node inspector live in {@link EngineWorkflowView}.
 *
 * This page shows the STRUCTURE ONLY — no run cursor, no blackboard. A run belongs to a Spec, so the
 * runtime view is opened from the Spec (its detail page / the dev overlay) through the run modal;
 * `/engine` answers "what does this workflow do", never "what is running". Authoring stays MCP-first
 * — nothing here mutates.
 */
export function EnginePage() {
  const { activeProjectId } = useActiveProject();
  const { workflows, loading: wfLoading } = useEngineWorkflows(activeProjectId);

  // No auto-select (finding #36): `null` = the home cards; an explicit pick opens that workflow's graph.
  const [selectedWorkflowId, setSelectedWorkflowId] = useState<string | null>(null);
  /** Follow a `sub_workflow` node: its target is named by SCOPE (`coder`, `prototyper`) — the same way
   *  the engine resolves it — or pinned by an explicit workflow id. Resolving it here keeps the page
   *  the single owner of "which workflow is open". */
  const openWorkflowTarget = useCallback(
    (target: { scope?: string; workflowId?: string }) => {
      const wf =
        (target.workflowId && workflows.find((w) => w.id === target.workflowId)) ||
        (target.scope && workflows.find((w) => w.scope === target.scope));
      if (wf) setSelectedWorkflowId(wf.id);
    },
    [workflows],
  );
  const selectedWorkflow = useMemo(
    () => workflows.find((w) => w.id === selectedWorkflowId) ?? null,
    [workflows, selectedWorkflowId],
  );

  return (
    <div className="flex h-svh flex-col overflow-hidden bg-background px-6 pb-6 pt-10">
      <header className="mb-4 shrink-0">
        <Breadcrumb className="mb-2">
          {selectedWorkflowId ? (
            <button
              type="button"
              onClick={() => setSelectedWorkflowId(null)}
              className={breadcrumbLinkClass}
            >
              Engine
            </button>
          ) : (
            <BreadcrumbCurrent>Engine</BreadcrumbCurrent>
          )}
          {selectedWorkflowId ? (
            <BreadcrumbCurrent>{selectedWorkflow?.name ?? "Workflow"}</BreadcrumbCurrent>
          ) : null}
        </Breadcrumb>
        <div className="flex flex-wrap items-center gap-3">
          <h1 className="flex items-center gap-2 font-heading text-xl font-semibold tracking-tight text-foreground">
            <WorkflowIcon className="size-5 text-primary" aria-hidden />
            Orchestration engine
          </h1>
          <Badge variant="muted" className="font-mono uppercase tracking-wide">
            read-only
          </Badge>
        </div>
      </header>

      {selectedWorkflowId == null ? (
        <EngineHome workflows={workflows} loading={wfLoading} onSelect={setSelectedWorkflowId} />
      ) : (
        <EngineWorkflowView
          key={selectedWorkflowId}
          workflowId={selectedWorkflowId}
          onOpenWorkflow={openWorkflowTarget}
        />
      )}
    </div>
  );
}

export default EnginePage;
