import {
  BaseEdge,
  EdgeLabelRenderer,
  getSmoothStepPath,
  type EdgeProps,
  type EdgeTypes,
} from "@xyflow/react";

import type { EngineEdgeData } from "./build-graph";

/**
 * The custom EDGE of the engine graph — the neutral, token-stroked SEQUENTIAL flow (start → … →
 * terminal), routed as an orthogonal smooth-step down the central column.
 *
 * A loop's reboucle is NOT drawn as a floating back-edge: the old lateral `reboucle` arc bowed 200px
 * out to the side and OVERFLOWED the loop box (its label clipped — finding #39). The loop-back is now
 * indicated on the loop's BOX header (`↺` + its `until` condition, `EngineGroupNode`), contained, so
 * there is a single flow edge variant here.
 */
function EngineEdge({
  id,
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
  markerEnd,
  data,
}: EdgeProps) {
  const d = (data ?? { variant: "flow" }) as EngineEdgeData;
  const [path, midX, midY] = getSmoothStepPath({
    sourceX,
    sourceY,
    targetX,
    targetY,
    sourcePosition,
    targetPosition,
    borderRadius: 12,
    offset: 12,
    // Run sideways along the CLEAR LANE computed after layout (`data.laneY`, N38 review) rather than
    // half-way down. ELK lays containers out but never treats them as obstacles, so the default
    // half-way run regularly crossed an unrelated node/box — and dropped the branch label on a box's
    // edge. The lane is picked from the real geometry (no leaf node in its span), so this generalises
    // instead of trading one collision for another the way a fixed offset did.
    ...(typeof d.laneY === "number" ? { centerY: d.laneY } : {}),
  });
  // The branch label sits at the MIDPOINT OF ITS OWN EDGE — always attached to the edge it annotates.
  // (Threading ELK's own computed label position was tried and reverted: it detached labels from their
  // edges and piled them up far from what they describe.)
  return (
    <>
      <BaseEdge
        id={id}
        path={path}
        markerEnd={markerEnd}
        data-variant={d.variant}
        className="!stroke-border"
        style={{ strokeWidth: 1.5 }}
      />
      {/* N18(b): a selector-branch edge shows its CONDITION, so the branching is legible in the
          flattened chain (the only thing the graph could not otherwise convey). */}
      {d.branchCondition ? (
        <EdgeLabelRenderer>
          <div
            // `z-10` lifts the chip ABOVE the edge paths: drawn under them it read as struck through
            // by its own edge. Opaque background + the lift = the condition is always legible.
            className="nodrag nopan pointer-events-none absolute z-10 rounded border border-primary/40 bg-background px-1.5 py-0.5 text-[10px] font-medium text-primary shadow-sm"
            style={{ transform: `translate(-50%, -50%) translate(${midX}px, ${midY}px)` }}
            data-testid="branch-condition"
          >
            {d.branchCondition}
          </div>
        </EdgeLabelRenderer>
      ) : null}
    </>
  );
}

/** The custom edge-type registry passed to `<ReactFlow edgeTypes>`. */
export const engineEdgeTypes: EdgeTypes = {
  engine: EngineEdge,
};

export { EngineEdge };
