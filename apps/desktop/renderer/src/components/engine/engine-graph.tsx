import { useCallback, useEffect, useMemo, useRef, type CSSProperties } from "react";
import {
  Background,
  BackgroundVariant,
  Controls,
  ReactFlow,
  useEdgesState,
  useNodesState,
  useReactFlow,
  type Node as RFNode,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";

import { cn } from "@/lib/utils";

import type { EngineRFEdge, EngineRFNode } from "./build-graph";
import { engineEdgeTypes } from "./engine-edges";
import { EngineInteractionContext, engineNodeTypes, type EngineMarker } from "./engine-nodes";

/**
 * Map React Flow's Controls (and a MiniMap, if ever added) `--xy-controls-*` / `--xy-minimap-*`
 * CSS variables onto the design-system TOKENS so the zoom/fit panel follows the theme in dark AND
 * light — the library ships a baked LIGHT default (`#fefefe` buttons) that reads as a white block on
 * the dark canvas otherwise (finding #29). Token vars ONLY — no literal colour/px. Set on the canvas
 * wrapper, they inherit down to `.react-flow__controls-button`.
 */
const CANVAS_THEME_VARS = {
  "--xy-controls-button-background-color": "var(--card)",
  "--xy-controls-button-background-color-hover": "var(--muted)",
  "--xy-controls-button-color": "var(--card-foreground)",
  "--xy-controls-button-color-hover": "var(--foreground)",
  "--xy-controls-button-border-color": "var(--border)",
  "--xy-minimap-background-color": "var(--card)",
  "--xy-minimap-node-background-color": "var(--muted-foreground)",
  "--xy-minimap-mask-background-color": "color-mix(in oklch, var(--background) 60%, transparent)",
} as CSSProperties;

/** The zoom the run modal opens at: close enough to read the node, wide enough to see its neighbours. */
const FOCUS_ZOOM = 0.9;

/**
 * FOLLOW THE RUN. Rendered INSIDE `<ReactFlow>` (so it may use `useReactFlow`), this pans the viewport
 * onto the node a run is parked on and re-centres — ANIMATED — every time that cursor MOVES. Watching
 * a live run then means watching the graph walk itself, instead of hunting for the highlight in a
 * 50-node tree. It moves only the CAMERA: no node, edge or layout is touched, so it cannot disturb the
 * laid-out graph (findings #34/#37).
 *
 * It re-centres on a CURSOR CHANGE only — never on every render — so a parked run leaves the user free
 * to pan around without the camera yanking back. With no cursor (the Engine page shows no run) it is
 * inert.
 */
function ViewportFollow({ nodeId, enabled }: { nodeId: string | null; enabled: boolean }) {
  const { getNode, getNodesBounds, setCenter, getZoom } = useReactFlow();
  const lastRef = useRef<string | null>(null);
  // The FIRST centring is a jump (the camera has no meaningful position yet); every later one is an
  // ANIMATED pan from wherever the user currently is.
  const settledRef = useRef(false);

  useEffect(() => {
    if (!enabled || !nodeId) {
      lastRef.current = null;
      return;
    }
    if (lastRef.current === nodeId) return;
    // ELK resolves the layout asynchronously, so on the first paint the node may not be placed yet —
    // retry briefly rather than give up on the very move we most want to show.
    let tries = 0;
    let timer = 0;
    const centre = () => {
      // `getNodesBounds` reports ABSOLUTE bounds — a node nested in a group carries a PARENT-RELATIVE
      // `position`, so centring on that raw value would aim at the wrong place for every node inside a
      // sequence or a loop (i.e. most of them).
      const node = getNode(nodeId);
      const bounds = node ? getNodesBounds([node]) : null;
      if (bounds && bounds.width > 0) {
        lastRef.current = nodeId;
        const first = !settledRef.current;
        settledRef.current = true;
        setCenter(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2, {
          // Opening JUMPS straight to a readable zoom on the cursor. Every later move KEEPS the zoom
          // the user is on and only pans: forcing a zoom each time made a step look like it restarted
          // from the fully zoomed-out fit instead of travelling from where the camera actually was.
          zoom: first ? FOCUS_ZOOM : getZoom(),
          duration: first ? 0 : 700,
        });
        return;
      }
      if (tries++ < 20) timer = window.setTimeout(centre, 60);
    };
    centre();
    return () => window.clearTimeout(timer);
  }, [nodeId, enabled, getNode, getNodesBounds, setCenter, getZoom]);

  return null;
}

/**
 * The React Flow CANVAS of the engine graph (v2.8 Phase 6). It renders the laid-out custom nodes
 * (per-type) + group containers (a loop's reboucle shown on its box header) + the sequential flow
 * edges from `build-graph`, wires the
 * clickable `{{sub_agent}}` / `{{check}}` markers through {@link EngineInteractionContext}, and opens
 * a TYPE-ADAPTED inspector when ANY behavior-tree node is clicked (finding #32; the synthetic
 * start/terminal pills stay inert). The Controls panel is themed onto the design-system tokens so it
 * follows dark/light (finding #29). Read-only: no drag/connect/edit (authoring is MCP-first).
 */
export function EngineGraph({
  nodes,
  edges,
  onMarker,
  onSelectNode,
  onHoverNode,
  currentNodeId = null,
  followCurrent = false,
  className,
}: {
  nodes: EngineRFNode[];
  edges: EngineRFEdge[];
  onMarker: (marker: EngineMarker) => void;
  /** Select a node → open its type-adapted inspector (finding #32). Every real node is clickable;
   *  the synthetic start/terminal pills are not. */
  onSelectNode: (nodeId: string) => void;
  /** Cross-highlight hover (finding #35): a graph node reports its id (and `null` on leave) so the
   *  inspector's children list lights up the matching entry. */
  onHoverNode?: (nodeId: string | null) => void;
  /** The node a RUN is parked on. When `followCurrent` is set, the camera centres on it and re-centres
   *  (animated) each time it moves — the "watch the run walk" mode the run modal opens in. */
  currentNodeId?: string | null;
  followCurrent?: boolean;
  className?: string;
}) {
  const interaction = useMemo(
    () => ({ onMarker, onSelectNode, onHoverNode }),
    [onMarker, onSelectNode, onHoverNode],
  );

  // React Flow OWNS the node/edge state so it keeps each node's `measured` dimensions across renders.
  // The overlay (hover / selection / runtime cursor) hands a FRESH `nodes` array every interaction;
  // passing it straight to <ReactFlow> would make React Flow re-measure every node (a fresh array has
  // no `measured`) and — with `fitView` — RE-FIT the viewport on every hover (finding #37: the flicker /
  // zoom-jump). So we PATCH only the changed nodes' `data` onto React Flow's own measured nodes while
  // the id-set is unchanged, and only REPLACE the set (letting `fitView` re-fit once) when the graph's
  // STRUCTURE actually changes.
  const [rfNodes, setRfNodes, onNodesChange] = useNodesState<EngineRFNode>([...nodes]);
  const [rfEdges, setRfEdges] = useEdgesState<EngineRFEdge>([...edges]);
  const idSetRef = useRef<string>(nodes.map((n) => n.id).join("|"));

  useEffect(() => {
    const ids = nodes.map((n) => n.id).join("|");
    if (ids === idSetRef.current) {
      // Same structure → patch ONLY the nodes whose `data` changed (the overlay flags), reusing every
      // other node object untouched so React Flow never loses `measured` and never re-fits.
      const incoming = new Map(nodes.map((n) => [n.id, n]));
      setRfNodes((prev) =>
        prev.map((p) => {
          const inc = incoming.get(p.id);
          if (!inc || inc.data === p.data) return p;
          return { ...p, data: inc.data };
        }),
      );
    } else {
      idSetRef.current = ids;
      setRfNodes([...nodes]); // structure changed → replace; the initial measure + fitView re-runs once
    }
  }, [nodes, setRfNodes]);

  useEffect(() => {
    setRfEdges([...edges]);
  }, [edges, setRfEdges]);

  const handleNodeClick = useCallback(
    (_event: React.MouseEvent, node: RFNode) => {
      // Every behavior-tree node (leaf/decision + composite group) opens its inspector; the
      // synthetic start/terminal pills carry no config, so they stay inert.
      if (node.type === "enginePill") return;
      onSelectNode(node.id);
    },
    [onSelectNode],
  );

  return (
    <EngineInteractionContext.Provider value={interaction}>
      <div
        className={cn("h-full w-full", className)}
        data-testid="engine-canvas"
        style={CANVAS_THEME_VARS}
      >
        <ReactFlow
          nodes={rfNodes}
          edges={rfEdges}
          onNodesChange={onNodesChange}
          nodeTypes={engineNodeTypes}
          edgeTypes={engineEdgeTypes}
          onNodeClick={handleNodeClick}
          // Following FRAMES THE CURSOR itself, so the whole-graph fit is skipped: fitting first and
          // then flying to the node is what made a step look like it started from the zoomed-out view.
          fitView={!followCurrent}
          fitViewOptions={{ padding: 0.2 }}
          minZoom={0.2}
          maxZoom={1.75}
          nodesDraggable={false}
          nodesConnectable={false}
          edgesFocusable={false}
          proOptions={{ hideAttribution: true }}
        >
          <ViewportFollow nodeId={currentNodeId} enabled={followCurrent} />
          <Background variant={BackgroundVariant.Dots} gap={22} size={1} className="opacity-60" />
          <Controls showInteractive={false} className="!border-border !bg-card" />
        </ReactFlow>
      </div>
    </EngineInteractionContext.Provider>
  );
}
