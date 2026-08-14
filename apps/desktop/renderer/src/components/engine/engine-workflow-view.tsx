import { WorkflowGraphView } from "./workflow-graph-view";

/**
 * The engine WORKFLOW VIEW — the graph for ONE explicitly-picked workflow (finding #36 opens on this
 * from the home cards). It is the SHARED {@link WorkflowGraphView} used with NO RUN: `/engine`
 * describes the orchestration STRUCTURE (the behavior tree + its node inspector), nothing else.
 *
 * The runtime — a run's cursor and its blackboard — moved to where the work actually is: the Spec
 * (its detail page + the dev overlay open the run modal). `/engine` used to overlay the workflow's
 * LATEST run here, which surfaced a long-finished run as if something were executing; the Engine page
 * is now purely structural and a run is only ever shown from the Spec that owns it.
 */
export function EngineWorkflowView({
  workflowId,
  onOpenWorkflow,
}: {
  workflowId: string;
  /** Follow a `sub_workflow` node to the flow it runs. `/engine` has NO run, so "descend into the child
   *  run" is meaningless here — what it can do is open that flow's STRUCTURE, which is the page's job. */
  onOpenWorkflow?: (target: { scope?: string; workflowId?: string }) => void;
}) {
  // The page (unlike the modal, which IS the frame) keeps the card treatment it always had.
  return (
    <WorkflowGraphView
      workflowId={workflowId}
      className="rounded-xl border border-border"
      onEnterSubWorkflow={
        onOpenWorkflow ? (t) => onOpenWorkflow({ scope: t.scope, workflowId: t.workflowId }) : undefined
      }
      enterSubWorkflowLabel="Open this flow"
    />
  );
}
