import { useEffect, useMemo, useRef, useState } from "react";
import { MousePointerClickIcon, WorkflowIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Spinner } from "@/components/ui/spinner";
import type { EngineWorkflowRun } from "@/bridge";

import {
  applyEngineOverlay,
  buildFlowGraph,
  graphStructureKey,
  parseNodeConfig,
  type EngineFlowGraph,
} from "./build-graph";
import { BlackboardPanel } from "./blackboard-panel";
import { EngineGraph } from "./engine-graph";
import type { EngineMarker } from "./engine-nodes";
import { NodeConfigInspector } from "./node-config-inspector";
import { isKnownAgentRole, NodeInspector } from "./node-inspector";
import { useEffectorContext, useEngineGraph } from "./use-engine";

/**
 * `<WorkflowGraphView>` — the SHARED workflow graph viewer. It owns the STATIC graph (ELK compound
 * layout — finding #34), the graph↔inspector cross-highlight (#35) and the TYPE-ADAPTED NODE
 * INSPECTOR (click ANY node → its config; a spawn node → the rendered sub-agent context). Layout +
 * the edge model are OWNED by `build-graph` — this view only re-flags the cheap overlay.
 *
 * The RUN is OPTIONAL, and that is the whole point of the split:
 *  - NO `run` (the `/engine` page) → the PURE STRUCTURE. No cursor, no blackboard, nothing that
 *    could make a finished run read as something executing right now. The right rail idles on a
 *    "pick a node" hint until a node is selected.
 *  - a `run` (the Spec-side run modal) → the run's `current_node_id` is highlighted on the graph and,
 *    with `showRuntimePanel`, the {@link BlackboardPanel} follows its variables in the right rail.
 *
 * The run also drives the inspector's effector context: `run.spec_id` renders the LIVE manual, its
 * absence the STATIC role template (finding #35).
 */
export function WorkflowGraphView({
  workflowId,
  onWorkflowName,
  onEnterSubWorkflow,
  enterSubWorkflowLabel,
  run = null,
  showRuntimePanel = false,
  className,
}: {
  workflowId: string;
  /** Reports the loaded workflow's NAME to the host (the run modal titles itself with it), so the name
   *  is read ONCE from the graph already being fetched rather than re-queried. */
  onWorkflowName?: (name: string | null) => void;
  /** Where a `sub_workflow` node points. Reported so a host can FOLLOW it — the run modal descends into
   *  the CHILD RUN that node opened, `/engine` (which has no run) opens the target flow's STRUCTURE.
   *  Absent ⇒ the node just opens its inspector like any other. */
  onEnterSubWorkflow?: (target: {
    nodeId: string;
    scope?: string;
    workflowId?: string;
  }) => void;
  /** The label of the visible "follow it" action in the inspector — a hidden click is not discoverable,
   *  so the affordance is a real button and the host words it for what IT does. */
  enterSubWorkflowLabel?: string;
  /** The run to OVERLAY on the structure. Absent/null ⇒ a purely structural view. */
  run?: EngineWorkflowRun | null;
  /** Render the runtime blackboard in the right rail (only meaningful with a `run`). */
  showRuntimePanel?: boolean;
  className?: string;
}) {
  const { graph, loading: graphLoading, error: graphError } = useEngineGraph(workflowId);

  // NO run ⇒ no runtime overlay at all: no cursor node, and no Spec bound for the effector context.
  const currentNodeId = run?.current_node_id ?? null;
  const specId = run?.spec_id ?? null;

  // The node OPENED in the inspector (any type — finding #32). Reset when the workflow changes.
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  useEffect(() => setSelectedNodeId(null), [workflowId]);

  // The node HOVERED anywhere — the graph↔children CROSS-HIGHLIGHT (finding #35). Shared here so a
  // hover from the inspector's list flags the graph node, and a hover on a graph node flags the list.
  const [hoveredNodeId, setHoveredNodeId] = useState<string | null>(null);

  // LAYOUT (positions + sizes + hierarchy) is async — ELK returns a Promise (finding #34). It must
  // re-run ONLY when the graph's SHAPE actually changes, keyed on a STABLE structural signature — NOT
  // on the `graph` object's identity. `useEngineGraph` re-pulls live on coarse invalidations, handing
  // back a fresh-but-identical object; keyed on that reference, ELK would re-lay-out on every
  // invalidation and — mid-Promise — leave the OLD and NEW positions co-existing (the N37a flicker /
  // "node in two places"). Keyed on the signature string, an identical re-pull is a no-op. Interaction
  // state (hover / selection / cursor) NEVER feeds the layout — it rides the cheap overlay below.
  const [layout, setLayout] = useState<EngineFlowGraph>({ nodes: [], edges: [] });
  useEffect(() => {
    onWorkflowName?.(graph?.workflow.name ?? null);
  }, [graph?.workflow.name, onWorkflowName]);

  const structureKey = useMemo(
    () => (graph ? graphStructureKey(graph.nodes, graph.workflow.root_node_id) : null),
    [graph],
  );
  // Read the latest graph inside the effect WITHOUT keying on its identity — only the signature keys it.
  const graphRef = useRef(graph);
  graphRef.current = graph;
  useEffect(() => {
    const g = graphRef.current;
    if (!g) {
      setLayout({ nodes: [], edges: [] });
      return;
    }
    let stale = false;
    void buildFlowGraph(g.nodes, g.workflow.root_node_id).then((laid) => {
      if (!stale) setLayout(laid);
    });
    return () => {
      stale = true;
    };
  }, [structureKey]);

  // The runtime OVERLAY (current cursor + selected node) is a CHEAP pure re-map over the laid-out nodes
  // — no fresh ELK pass on every cursor tick (finding #34). Edges are unaffected by the overlay.
  const flow = useMemo(
    () => ({
      nodes: applyEngineOverlay(layout.nodes, currentNodeId, selectedNodeId, hoveredNodeId),
      edges: layout.edges,
    }),
    [layout, currentNodeId, selectedNodeId, hoveredNodeId],
  );

  const currentNodeLabel = useMemo(() => {
    if (!graph || !currentNodeId) return null;
    return graph.nodes.find((n) => n.id === currentNodeId)?.label ?? null;
  }, [graph, currentNodeId]);

  // The selected node's row + (for a composite) its ordered children.
  const selectedNode = useMemo(
    () =>
      graph && selectedNodeId ? (graph.nodes.find((n) => n.id === selectedNodeId) ?? null) : null,
    [graph, selectedNodeId],
  );
  const selectedChildren = useMemo(() => {
    if (!graph || !selectedNode) return [];
    return graph.nodes
      .filter((n) => n.parent_id === selectedNode.id)
      .slice()
      .sort((a, b) => a.order_index - b.order_index);
  }, [graph, selectedNode]);

  // A spawn node opens the sub-agent CONTEXT inspector (its `agent_id` drives `render_agent_context`);
  // every other type opens the type-adapted config inspector, which needs no async fetch.
  const inspectedRole =
    selectedNode?.type_ === "spawn_subagent"
      ? ((parseNodeConfig(selectedNode.config).agent_id as string | undefined) ?? null)
      : null;
  // …and its per-node `subject`, so the inspector shows the manual NARROWED the way the run
  // forwards it (an audit pass gets the ONE Spec section it is about, not all six).
  const inspectedSubject =
    selectedNode?.type_ === "spawn_subagent"
      ? ((parseNodeConfig(selectedNode.config).subject as string | undefined) ?? null)
      : null;
  // Only a KNOWN role has a renderable template; a custom/unknown agent_id (e.g. `dragon-slayer`)
  // would make `render_role_template` error, so we skip the fetch and the inspector shows a clear
  // custom state instead of a spurious "Could not render" (finding #40).
  const roleKnown = inspectedRole != null && isKnownAgentRole(inspectedRole);
  const {
    context,
    loading: ctxLoading,
    error: ctxError,
  } = useEffectorContext(roleKnown ? inspectedRole : null, specId, inspectedSubject);

  // A marker chip carries the node it belongs to (`nodeId`) — select it directly. The inspector's
  // in-text markers carry no node id (`nodeId === ""`), so map the role/command back to its node.
  const handleMarker = (marker: EngineMarker) => {
    if (marker.nodeId) {
      setSelectedNodeId(marker.nodeId);
      return;
    }
    const match = graph?.nodes.find((n) => {
      const cfg = parseNodeConfig(n.config);
      if (marker.kind === "sub_agent") return cfg.agent_id === marker.id;
      return Array.isArray(cfg.command_ids) && cfg.command_ids.includes(marker.id);
    });
    if (match) setSelectedNodeId(match.id);
  };

  // Selecting a node opens its inspector; a `sub_workflow` node ALSO tells the host, which can then
  // descend into the child run it opened (the run modal does).
  const handleSelectNode = (nodeId: string) => setSelectedNodeId(nodeId);

  /** The `sub_workflow` target of the SELECTED node, when it is one and the host can follow it. */
  const subWorkflowTarget = (() => {
    if (!onEnterSubWorkflow || !selectedNode || selectedNode.type_ !== "sub_workflow") return null;
    const cfg = parseNodeConfig(selectedNode.config);
    return {
      nodeId: selectedNode.id,
      scope: typeof cfg.scope === "string" ? cfg.scope : undefined,
      workflowId: typeof cfg.workflow_id === "string" ? cfg.workflow_id : undefined,
    };
  })();

  // The runtime rail is opt-in AND run-gated: with no run there is nothing to show, so the blackboard
  // is not rendered at all (rather than a panel narrating "no run").
  const runtimeRail = showRuntimePanel && run != null;

  return (
    <div
      className={cn(
        // No frame of its own: the viewer fills whatever holds it. Inside the run modal a rounded,
        // bordered card read as a component pasted INTO the dialog rather than as the dialog's content;
        // a host that does want a frame passes it through `className`.
        "grid min-h-0 flex-1 grid-cols-[minmax(0,1fr)_380px] overflow-hidden bg-card/20",
        className,
      )}
    >
      {/* The graph canvas. */}
      <div className="relative min-h-0 min-w-0">
        {graphLoading ? (
          <div className="flex h-full items-center justify-center">
            <Spinner />
          </div>
        ) : graphError ? (
          <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
            Could not load the workflow graph.
          </div>
        ) : flow.nodes.length === 0 ? (
          <div className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center text-muted-foreground">
            <WorkflowIcon className="size-6 opacity-40" />
            <p className="text-sm">This workflow has no nodes.</p>
          </div>
        ) : (
          <EngineGraph
            nodes={flow.nodes}
            edges={flow.edges}
            onMarker={handleMarker}
            onSelectNode={handleSelectNode}
            onHoverNode={setHoveredNodeId}
            // With a run on screen the camera FOLLOWS its cursor: it opens centred on the node the run
            // is parked on and re-centres, animated, at every step — so a live run reads as the graph
            // walking itself. Without a run (the Engine page) there is no cursor and nothing follows.
            currentNodeId={currentNodeId}
            followCurrent={run != null}
          />
        )}
      </div>

      {/* The right rail: the TYPE-ADAPTED inspector for the selected node — the sub-agent context for
          a spawn, its config for every other type (finding #32). With nothing selected it shows the
          run's blackboard (runtime views only), else the pick-a-node hint. */}
      <div className="min-h-0 border-l border-border bg-card">
        {selectedNode == null ? (
          runtimeRail ? (
            <BlackboardPanel run={run} currentNodeLabel={currentNodeLabel} />
          ) : (
            <div
              data-testid="graph-inspector-hint"
              className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center text-muted-foreground"
            >
              <MousePointerClickIcon className="size-5 opacity-40" aria-hidden />
              <p className="text-2xs">Pick a node to inspect what it does.</p>
            </div>
          )
        ) : selectedNode.type_ === "spawn_subagent" ? (
          <NodeInspector
            agentId={inspectedRole ?? selectedNode.label}
            context={context}
            bound={specId != null}
            builtIn={graph?.workflow.project_id == null}
            loading={ctxLoading}
            error={ctxError}
            onMarker={handleMarker}
            onClose={() => setSelectedNodeId(null)}
          />
        ) : (
          <NodeConfigInspector
            node={selectedNode}
            childNodes={selectedChildren}
            allNodes={graph?.nodes}
            onSelectNode={handleSelectNode}
            onClose={() => setSelectedNodeId(null)}
            hoveredNodeId={hoveredNodeId}
            onHoverNode={setHoveredNodeId}
            followAction={
              subWorkflowTarget
                ? {
                    label: enterSubWorkflowLabel ?? "Open this flow",
                    onFollow: () => onEnterSubWorkflow?.(subWorkflowTarget),
                  }
                : undefined
            }
          />
        )}
      </div>
    </div>
  );
}
