import ELK, { type ElkNode, type LayoutOptions } from "elkjs/lib/elk.bundled.js";
import type { Edge, Node } from "@xyflow/react";

import type { EngineNode } from "@/bridge";

import { isGroupType, type EngineNodeType } from "./node-meta";

/**
 * Turn a workflow's FLAT node rows into a laid-out React Flow graph (v2.8 Phase 6).
 *
 * This is the PURE core of the engine view — no React, no DOM — so it is exhaustively unit-testable:
 *   1. RECONSTRUCT the behavior tree from the rows (`parent_id` + `order_index`), exactly like
 *      nyx-core's `engine::Tree::from_rows`.
 *   2. Emit the flow: COMPOSITE `loop`/`sequence` become GROUP container nodes; every other node is a
 *      custom FLOW node; two synthetic PILLS bracket the run (`start` → the entry, `terminal` → end).
 *   3. Wire the edges: the SEQUENTIAL execution flow between leaf nodes. A `loop`'s reboucle is NOT a
 *      floating back-edge (it overflowed the box — finding #39); instead the loop's `until` condition
 *      is surfaced on its GROUP header as a `↺` loop-back indicator (`EngineGroupData.loopBack`).
 *   4. LAY IT OUT with ELK as a COMPOUND graph (finding #34): the group containers are ELK nodes whose
 *      `children` are their flow nodes and nested groups, so ELK sizes each box to CONTAIN its content
 *      and routes the layout AROUND the boxes — no more post-hoc min/max derivation that let group
 *      boxes overlap and edges cross through them. This nesting is what the future sub-workflows (one
 *      more level) build on. Layout is async (`elk.layout` returns a Promise).
 *
 * The runtime overlay is a pure function of `currentNodeId` / `selectedNodeId`: {@link applyEngineOverlay}
 * re-flags the laid-out nodes WITHOUT re-running ELK, so a cursor tick never triggers a fresh layout.
 */

/** The engine's single ELK instance (the bundled, worker-less browser build — no CDN/worker setup). */
const elk = new ELK();

/** Fixed node dimensions ELK lays out with (the real DOM size is close; exactness is not needed for
 *  a readable tree — ELK only needs consistent boxes). */
const LEAF_WIDTH = 254;
const LEAF_HEIGHT = 70;
const PILL_WIDTH = 210;
const PILL_HEIGHT = 42;

/** The inset of a group's children within its box. ELK applies it per container, so nested groups stack
 *  it natively — no manual per-level padding. */
const GROUP_PADDING = "[top=32,left=32,bottom=32,right=32]";

/** Layered, top-to-bottom compound layout (finding #34). `hierarchyHandling: INCLUDE_CHILDREN` treats the
 *  whole nesting as one layout pass, so flow edges may cross group boundaries and ELK still contains and
 *  spaces the boxes. Options are inherited by every nested container.
 *
 *  The spacing is deliberately GENEROUS (N38): the branch-condition edge LABELS (rendered by React Flow,
 *  which ELK does not otherwise see) and the group BOXES used to overlap edges/nodes at tighter spacing.
 *  `edgeNode` / `edgeNodeBetweenLayers` keep an edge (and its label) clear of the boxes it routes past,
 *  and `edgeLabels.placement: CENTER` + `spacing.edgeLabel` make ELK RESERVE the space each branch label
 *  needs (its size is declared on the ELK edge below), so a label no longer sits on top of another edge. */
const LAYOUT_OPTIONS: LayoutOptions = {
  "elk.algorithm": "layered",
  "elk.direction": "DOWN",
  "elk.layered.spacing.nodeNodeBetweenLayers": "80",
  "elk.spacing.nodeNode": "64",
  "elk.spacing.edgeNode": "32",
  "elk.layered.spacing.edgeNodeBetweenLayers": "32",
  "elk.edgeLabels.placement": "CENTER",
  "elk.spacing.edgeLabel": "8",
  "elk.padding": GROUP_PADDING,
  "elk.hierarchyHandling": "INCLUDE_CHILDREN",
};

/** The id of the synthetic ELK root — never a workflow node id. */
const ELK_ROOT_ID = "__engine_elk_root__";

/** The id of the synthetic START / TERMINAL pills (never collide with a UUIDv7 node id). */
export const START_ID = "__engine_start__";
export const TERMINAL_ID = "__engine_terminal__";

/** The `data` a custom FLOW node renders from. */
export interface EngineFlowData extends Record<string, unknown> {
  readonly nodeType: EngineNodeType;
  readonly label: string;
  readonly nodeId: string;
  /** `spawn_subagent` — the effector role it launches (its `{{sub_agent:id}}` marker target). */
  readonly agentId?: string;
  /** `gate_check` — the managed-command ids it runs (its `{{check:id}}` markers). */
  readonly commandIds?: readonly string[];
  /** Runtime: this is the run's CURRENT node (highlight it). */
  readonly current: boolean;
  /** Interaction: this node is the one OPENED in the inspector (selected state — finding #33). */
  readonly selected: boolean;
  /** Interaction: this node is HOVERED — either directly on the canvas or via its entry in the
   *  inspector's children list (the cross-highlight of finding #35). A cheap overlay flag, no relayout. */
  readonly hovered: boolean;
}

/** The `data` a GROUP container (loop / sequence) renders from. */
export interface EngineGroupData extends Record<string, unknown> {
  readonly nodeType: "loop" | "sequence";
  readonly label: string;
  readonly nodeId: string;
  /** The workflow name (the START pill's label), so a group whose own label merely ECHOES it is not
   *  re-shown — the name lives ONCE on the start pill, the group chip keeps only its type. */
  readonly rootLabel: string;
  /** Interaction: this group is the one OPENED in the inspector (selected state — finding #33). */
  readonly selected: boolean;
  /** Interaction: this group is HOVERED (cross-highlight with the inspector's children — finding #35). */
  readonly hovered: boolean;
  /** A `loop`'s READABLE exit condition, derived from its `until` config (e.g. `phase_status =
   *  complete`) — shown on the loop's header as the `↺` loop-back indicator (finding #39). `null`
   *  for a `sequence`, or a loop whose `until` is absent/undescribable. */
  readonly loopBack: string | null;
}

/** The `data` a START / TERMINAL pill renders from. */
export interface EnginePillData extends Record<string, unknown> {
  readonly nodeType: "start" | "terminal";
  readonly label: string;
}

/** The custom-edge `data`. The engine graph draws the neutral sequential FLOW between leaves; a loop's
 *  reboucle is indicated on the loop's BOX header (finding #39), never as a floating edge. An edge that
 *  ENTERS a `selector` branch carries that branch's CONDITION (N18(b)) — the branching is otherwise
 *  invisible in the flattened chain. */
export interface EngineEdgeData extends Record<string, unknown> {
  readonly variant: "flow";
  /** For an edge entering a selector branch: the readable condition under which that branch is taken
   *  (e.g. `spec_status = ready`, `always`) — every condition routing there, OR-ed, when the selector
   *  aims several branches at the one child. Absent on a plain sequential edge. */
  readonly branchCondition?: string;
  /** The Y of the edge's HORIZONTAL run, chosen after layout as a lane that crosses NO node (N38
   *  review). Absent when the edge needs no sideways run (a straight drop) or no clear lane exists. */
  readonly laneY?: number;
}

export type EngineRFNode = Node<EngineFlowData | EngineGroupData | EnginePillData>;
export type EngineRFEdge = Edge<EngineEdgeData>;

/** The built, laid-out graph handed to `<ReactFlow>`. */
export interface EngineFlowGraph {
  readonly nodes: EngineRFNode[];
  readonly edges: EngineRFEdge[];
}

/** A reconstructed tree node (arena-free, parent/child pointers). */
interface TreeNode {
  readonly raw: EngineNode;
  readonly type: string;
  readonly children: TreeNode[];
  parent: TreeNode | null;
}

/** Reconstruct the pure tree from the flat rows (root = `rootId`, else the `parent_id IS NULL` row).
 *  Children are ordered by `order_index`. Returns `null` for an empty workflow. */
function reconstruct(nodes: readonly EngineNode[], rootId: string | null): TreeNode | null {
  if (nodes.length === 0) return null;
  const byId = new Map<string, TreeNode>();
  for (const raw of nodes) {
    byId.set(raw.id, { raw, type: raw.type_, children: [], parent: null });
  }
  // Wire children onto their parent in `order_index` order (a stable sort by index).
  const ordered = [...nodes].sort((a, b) => a.order_index - b.order_index);
  let fallbackRoot: TreeNode | null = null;
  for (const raw of ordered) {
    const tn = byId.get(raw.id)!;
    if (raw.parent_id == null) {
      fallbackRoot ??= tn;
    } else {
      const parent = byId.get(raw.parent_id);
      if (parent) {
        parent.children.push(tn);
        tn.parent = parent;
      }
    }
  }
  if (rootId && byId.has(rootId)) return byId.get(rootId)!;
  return fallbackRoot;
}

/** The nearest STRICT ancestor that is a GROUP composite (loop / sequence), i.e. the React Flow
 *  container this node lives inside — or `null` when it is top-level. */
function nearestGroup(tn: TreeNode): TreeNode | null {
  let p = tn.parent;
  while (p) {
    if (isGroupType(p.type)) return p;
    p = p.parent;
  }
  return null;
}

/** The FLOW nodes (non-group) inside a subtree, in execution (pre-)order. A group is descended into
 *  but never itself emitted; a flow node is emitted, then its own children (e.g. selector branches). */
function collectFlow(tn: TreeNode, out: TreeNode[] = []): TreeNode[] {
  if (isGroupType(tn.type)) {
    for (const c of tn.children) collectFlow(c, out);
  } else {
    out.push(tn);
    for (const c of tn.children) collectFlow(c, out);
  }
  return out;
}

/** Every node of a subtree (groups included), pre-order. */
function collectAll(tn: TreeNode, out: TreeNode[] = []): TreeNode[] {
  out.push(tn);
  for (const c of tn.children) collectAll(c, out);
  return out;
}

/** The group-nesting DEPTH of a node — how many GROUP ancestors it has (0 = top-level). Orders the
 *  emitted group nodes shallowest-first, so React Flow always sees a parent container BEFORE its
 *  children. */
function groupDepth(tn: TreeNode): number {
  let depth = 0;
  let p = tn.parent;
  while (p) {
    if (isGroupType(p.type)) depth += 1;
    p = p.parent;
  }
  return depth;
}

/** An execution-flow edge: a source→target pair, optionally carrying the readable condition of the
 *  selector branch it enters (the N18(b) label). Feeds BOTH ELK's ranking and the drawn edges. */
interface FlowEdge {
  source: string;
  target: string;
  branchCondition?: string;
}

/** The flow-node ids that ENTER and EXIT a subtree — the seams the enclosing flow wires onto. Empty
 *  for a composite that holds no flow node (an empty `loop`/`sequence`/`selector`). */
interface Endpoints {
  entries: string[];
  exits: string[];
}

/** Wire a subtree's INTERNAL execution edges into `edges` and return its entry/exit seams (N33):
 *   - a GROUP (`loop`/`sequence`) chains its children in order (exit → next entry);
 *   - a SELECTOR FANS OUT — `selector → each branch entry` (labelled with the branch condition) — and
 *     its exits are the UNION of the branch tails, so the enclosing flow JOINS every branch onto the
 *     selector's successor. Branches are SPREAD, never chained into one vertical column.
 *   - a plain leaf is its own single entry and exit.
 *  This replaces the old flat pre-order chain that read three alternatives as a sequence. */
function wireSubtree(tn: TreeNode, edges: FlowEdge[]): Endpoints {
  if (isGroupType(tn.type)) return wireSequence(tn.children, edges);

  if (tn.type === "selector" && tn.children.length > 0) {
    const entries = [tn.raw.id];
    const exits: string[] = [];
    tn.children.forEach((child, childIndex) => {
      const inner = wireSubtree(child, edges);
      if (inner.entries.length === 0) return; // an empty branch draws nothing
      const branchCondition = describeBranchCondition(tn.raw.config, childIndex) ?? undefined;
      for (const entry of inner.entries) edges.push({ source: tn.raw.id, target: entry, branchCondition });
      exits.push(...inner.exits);
    });
    // Every branch empty ⇒ the selector is a lone node (its own seam).
    if (exits.length === 0) return { entries: [tn.raw.id], exits: [tn.raw.id] };
    return { entries, exits };
  }

  // A leaf (or a childless selector): its own single seam.
  return { entries: [tn.raw.id], exits: [tn.raw.id] };
}

/** Chain a composite's children in execution order, skipping any that hold no flow. Returns the first
 *  child's entries and the last child's exits (empty seams when nothing flows). */
function wireSequence(children: readonly TreeNode[], edges: FlowEdge[]): Endpoints {
  const parts = children
    .map((c) => wireSubtree(c, edges))
    .filter((p) => p.entries.length > 0 && p.exits.length > 0);
  for (let i = 0; i < parts.length - 1; i += 1) {
    for (const source of parts[i]!.exits) {
      for (const target of parts[i + 1]!.entries) edges.push({ source, target });
    }
  }
  if (parts.length === 0) return { entries: [], exits: [] };
  return { entries: parts[0]!.entries, exits: parts[parts.length - 1]!.exits };
}

/** Build the laid-out React Flow graph for a workflow's nodes (ASYNC — ELK's `layout` is a Promise,
 *  finding #34). `currentNodeId` highlights the runtime cursor, `selectedNodeId` the inspected node
 *  (pass `null` for the static view; the page re-flags them cheaply via {@link applyEngineOverlay}
 *  instead of re-running ELK on every tick). */
export async function buildFlowGraph(
  nodes: readonly EngineNode[],
  rootId: string | null,
  currentNodeId: string | null = null,
  selectedNodeId: string | null = null,
): Promise<EngineFlowGraph> {
  const root = reconstruct(nodes, rootId);
  if (!root) return { nodes: [], edges: [] };

  const all = collectAll(root);
  const flowOrder = collectFlow(root);
  // An EMPTY composite (a `loop` / `sequence` with no leaf under it) has nothing to contain, so it is
  // not drawn — it would otherwise be a zero-child ELK box collapsing onto the start pill. (Dropping it
  // is safe: a flow node's nearest group always contains at least that flow node, and so does every
  // group above it, so no emitted node is ever orphaned.)
  const groups = all.filter((tn) => isGroupType(tn.type) && collectFlow(tn).length > 0);

  // The execution flow the graph draws AND ELK ranks by. A sequence chains its children, but a
  // SELECTOR fans out to each branch and the branches CONVERGE on the selector's successor (N33) — so
  // three alternatives read as a fork, not a vertical chain. (A loop's reboucle is a header indicator,
  // not an edge — finding #39.) START brackets the root's entries; the root's exits reach TERMINAL.
  const internalEdges: FlowEdge[] = [];
  const rootEnds = wireSubtree(root, internalEdges);
  const flowGraphEdges: FlowEdge[] =
    rootEnds.entries.length === 0
      ? [{ source: START_ID, target: TERMINAL_ID }] // a root with no flow node at all
      : [
          ...rootEnds.entries.map((entry) => ({ source: START_ID, target: entry })),
          ...internalEdges,
          ...rootEnds.exits.map((exit) => ({ source: exit, target: TERMINAL_ID })),
        ];

  // --- ELK COMPOUND graph MIRRORING the group hierarchy (finding #34) -----------------------------
  // Each non-empty group is an ELK container whose `children` are its flow nodes AND nested group
  // containers; leaves and the two pills are sized ELK nodes. ELK sizes every container to CONTAIN its
  // children (with `GROUP_PADDING`) and lays the whole nesting out in one layered pass, so boxes never
  // overlap and edges route around them. The parent a node lives in is exactly its `nearestGroup` — the
  // same relation React Flow's `parentId` uses — so the two hierarchies stay in lock-step.
  const elkGroupById = new Map<string, ElkNode>();
  for (const grp of groups) {
    elkGroupById.set(grp.raw.id, {
      id: grp.raw.id,
      layoutOptions: { "elk.padding": GROUP_PADDING },
      children: [],
    });
  }
  const elkRoot: ElkNode = { id: ELK_ROOT_ID, layoutOptions: LAYOUT_OPTIONS, children: [], edges: [] };
  // The ELK children array a tree node's box belongs to: its nearest GROUP container, else the root.
  const containerOf = (tn: TreeNode): ElkNode[] => {
    const grp = nearestGroup(tn);
    return grp ? elkGroupById.get(grp.raw.id)!.children! : elkRoot.children!;
  };
  for (const grp of groups) containerOf(grp).push(elkGroupById.get(grp.raw.id)!);
  for (const tn of flowOrder) {
    containerOf(tn).push({ id: tn.raw.id, width: LEAF_WIDTH, height: LEAF_HEIGHT });
  }
  // The pills bracket the whole run — top-level ELK nodes, never inside a group box.
  elkRoot.children!.push({ id: START_ID, width: PILL_WIDTH, height: PILL_HEIGHT });
  elkRoot.children!.push({ id: TERMINAL_ID, width: PILL_WIDTH, height: PILL_HEIGHT });
  // The flow edges — held at the ELK root; `INCLUDE_CHILDREN` lets them cross group boundaries. ELK
  // only ranks/positions with them; React Flow draws its own edges (below) from the SAME list. A
  // branch edge declares its RF label as an ELK label (size ≈ the rendered chip) so ELK RESERVES the
  // space for it and reports back where it PLACED it (N38) — otherwise ELK is blind to the label and
  // lets it overlap a neighbouring edge/node.
  elkRoot.edges = flowGraphEdges.map((e, i) => ({
    id: `elk-${i}`,
    sources: [e.source],
    targets: [e.target],
    ...(e.branchCondition
      ? {
          labels: [
            {
              id: `elk-${i}-label`,
              text: e.branchCondition,
              width: e.branchCondition.length * 6.5 + 12,
              height: 16,
            },
          ],
        }
      : {}),
  }));

  const laid = await elk.layout(elkRoot);

  // NOTE (N38, reverted): ELK also REPORTS where it placed each edge label, but threading those
  // coordinates into the rendered label detached it from its own edge (labels piled up far from the
  // edge they annotate). The label is drawn at the MIDPOINT OF ITS OWN EDGE (`engine-edges.tsx`) —
  // always attached to what it describes. ELK still gets the label SIZE above, so it reserves the room.

  // Every laid-out box, keyed by id. ELK positions children RELATIVE to their parent container — which
  // is exactly how React Flow reads a child's `position` against its `parentId`, so no remapping.
  const box = new Map<string, { x: number; y: number; width: number; height: number }>();
  const walk = (node: ElkNode) => {
    for (const child of node.children ?? []) {
      box.set(child.id, {
        x: child.x ?? 0,
        y: child.y ?? 0,
        width: child.width ?? 0,
        height: child.height ?? 0,
      });
      walk(child);
    }
  };
  walk(laid);
  const at = (id: string) => box.get(id) ?? { x: 0, y: 0, width: 0, height: 0 };
  /** ELK (like React Flow) positions a child RELATIVE to its container; the lane scan below needs one
   *  common frame, so walk the container chain and sum the offsets. */
  const absOf = (tn: TreeNode) => {
    let x = at(tn.raw.id).x;
    let y = at(tn.raw.id).y;
    let g = nearestGroup(tn);
    while (g) {
      const gb = at(g.raw.id);
      x += gb.x;
      y += gb.y;
      g = nearestGroup(g);
    }
    return { x, y };
  };
  const absX = (tn: TreeNode) => absOf(tn).x;
  const absY = (tn: TreeNode) => absOf(tn).y;

  const rfNodes: EngineRFNode[] = [];

  // GROUPS first (shallowest → deepest) so a parent container is always emitted before its children.
  const sortedGroups = [...groups].sort((a, b) => groupDepth(a) - groupDepth(b));
  for (const grp of sortedGroups) {
    const b = at(grp.raw.id);
    const parent = nearestGroup(grp);
    rfNodes.push({
      id: grp.raw.id,
      type: "engineGroup",
      position: { x: b.x, y: b.y },
      data: {
        nodeType: grp.type as "loop" | "sequence",
        label: grp.raw.label,
        nodeId: grp.raw.id,
        rootLabel: root.raw.label,
        selected: selectedNodeId === grp.raw.id,
        hovered: false,
        loopBack: grp.type === "loop" ? describeLoopCondition(grp.raw.config) : null,
      },
      ...(parent ? { parentId: parent.raw.id, extent: "parent" as const } : {}),
      // The container's React Flow WRAPPER is transparent to pointer events (N37b): a group node's
      // wrapper spans its whole box and would otherwise SWALLOW clicks/hovers meant for the child nodes
      // sitting inside it. `pointerEvents:none` lets those events fall through to the children; the
      // group's own corner CHIP re-enables `pointer-events:auto` on itself, so the composite stays
      // selectable by its label. `zIndex:0` keeps the container BEHIND its (default-higher) children.
      style: { width: b.width, height: b.height, pointerEvents: "none" },
      zIndex: 0,
      draggable: false,
      selectable: false,
    });
  }

  // FLOW nodes.
  for (const tn of flowOrder) {
    const b = at(tn.raw.id);
    const parent = nearestGroup(tn);
    const config = parseNodeConfig(tn.raw.config);
    rfNodes.push({
      id: tn.raw.id,
      type: "engineFlow",
      position: { x: b.x, y: b.y },
      data: {
        nodeType: tn.type as EngineNodeType,
        label: tn.raw.label,
        nodeId: tn.raw.id,
        agentId: typeof config.agent_id === "string" ? config.agent_id : undefined,
        commandIds: Array.isArray(config.command_ids)
          ? config.command_ids.filter((c): c is string => typeof c === "string")
          : undefined,
        current: currentNodeId === tn.raw.id,
        selected: selectedNodeId === tn.raw.id,
        hovered: false,
      },
      ...(parent ? { parentId: parent.raw.id, extent: "parent" as const } : {}),
      // Stacking (N38-review): group boxes (0) < EDGES (1) < leaf nodes (2). A container used to paint
      // OVER the edges routed across it, so a line vanished behind a loop's box and the flow became
      // impossible to follow; the leaves stay on top so an edge never crosses a node's face.
      zIndex: 2,
      draggable: false,
    });
  }

  // PILLS (top-level, no parent).
  const startBox = at(START_ID);
  const terminalBox = at(TERMINAL_ID);
  rfNodes.push({
    id: START_ID,
    type: "enginePill",
    position: { x: startBox.x, y: startBox.y },
    data: { nodeType: "start", label: root.raw.label || "start" },
    draggable: false,
    selectable: false,
  });
  rfNodes.push({
    id: TERMINAL_ID,
    type: "enginePill",
    position: { x: terminalBox.x, y: terminalBox.y },
    data: { nodeType: "terminal", label: "terminal" },
    draggable: false,
    selectable: false,
  });

  // --- edges: the execution flow (selector fan-out + convergence — N33) ---------------------------
  // A loop's reboucle is shown on its BOX header (the `↺` loop-back indicator, `loopBack` above), not
  // as a floating back-edge (finding #39). An edge ENTERING a selector branch carries that branch's
  // readable condition (N18(b)), attached at fan-out time in `wireSubtree`.
  // --- pick a CLEAR horizontal lane per edge (N38 review) ------------------------------------------
  // A smooth-step edge runs down, sideways, then down again. React Flow puts that sideways run half-way
  // between the two nodes, which regularly laid it straight across an unrelated NODE (and dropped the
  // branch label on a container's edge). A FIXED offset just moves the collision elsewhere, so the lane
  // is COMPUTED: scan the band between source and target and take the first Y whose horizontal span
  // hits no leaf node. Group boxes are NOT obstacles — a branch legitimately enters the box it targets;
  // it is crossing a NODE that reads as broken.
  const leafRects = flowOrder.map((tn) => {
    const b = at(tn.raw.id);
    return { x: absX(tn), y: absY(tn), w: b.width || LEAF_WIDTH, h: b.height || LEAF_HEIGHT, id: tn.raw.id };
  });
  const LANE_CLEARANCE = 14;
  /** The Y of a clear sideways run between `from` and `to`, or `undefined` when none is needed/found. */
  const clearLane = (fromId: string, toId: string): number | undefined => {
    const a = leafRects.find((r) => r.id === fromId);
    const b = leafRects.find((r) => r.id === toId);
    if (!a || !b) return undefined;
    const x1 = Math.min(a.x, b.x);
    const x2 = Math.max(a.x + a.w, b.x + b.w);
    // No sideways run needed when the two are already column-aligned.
    if (Math.abs(a.x + a.w / 2 - (b.x + b.w / 2)) < 2) return undefined;
    const top = a.y + a.h + LANE_CLEARANCE;
    const bottom = b.y - LANE_CLEARANCE;
    if (bottom <= top) return undefined;
    const blocked = (y: number) =>
      leafRects.some(
        (r) =>
          r.id !== fromId &&
          r.id !== toId &&
          y > r.y - LANE_CLEARANCE &&
          y < r.y + r.h + LANE_CLEARANCE &&
          x1 < r.x + r.w &&
          r.x < x2,
      );
    // Prefer a lane close to the SOURCE (the run then happens in the open band under the fork, where a
    // branch label reads cleanly), falling back down the band until one is free.
    for (let y = top; y <= bottom; y += 8) if (!blocked(y)) return y;
    return undefined;
  };

  const edges: EngineRFEdge[] = flowGraphEdges.map((e) => ({
    id: `flow-${e.source}-${e.target}`,
    source: e.source,
    target: e.target,
    type: "engine",
    zIndex: 1, // above the group containers (0), below the leaf nodes (2) — see the stacking note.
    data: {
      variant: "flow",
      ...(e.branchCondition ? { branchCondition: e.branchCondition } : {}),
      ...(() => {
        const laneY = clearLane(e.source, e.target);
        return laneY === undefined ? {} : { laneY };
      })(),
    },
  }));

  return { nodes: rfNodes, edges };
}

/**
 * Re-flag the runtime OVERLAY on already-laid-out nodes (finding #34) — `data.current` (the run cursor),
 * `data.selected` (the inspected node) and `data.hovered` (the graph↔children cross-highlight of
 * finding #35) — WITHOUT re-running ELK. Layout depends only on the graph STRUCTURE, so a cursor tick,
 * a selection change or a hover is a cheap pure re-map, not a fresh layout pass. Returns a new array,
 * reusing the node identity when its flags are unchanged (stable references).
 */
/**
 * A STABLE structural signature of a workflow graph: the content ELK lays out + the data the nodes
 * carry (id / parent / order / type / label / config) plus the root. Two structurally-IDENTICAL graphs
 * — e.g. `useEngineGraph`'s live re-pull handing back a fresh-but-equal object on a coarse invalidation
 * — yield the SAME string. The async ELK layout is keyed on THIS, so it re-runs only on a real shape or
 * content change, never on object-identity churn (which, mid-Promise, left the old + new positions
 * co-existing — the N37a flicker / "node in two places"). Pure + exported so it's unit-testable.
 */
export function graphStructureKey(nodes: readonly EngineNode[], rootId: string | null): string {
  return JSON.stringify({
    root: rootId,
    nodes: nodes.map((n) => [n.id, n.parent_id, n.order_index, n.type_, n.label, n.config]),
  });
}

export function applyEngineOverlay(
  nodes: readonly EngineRFNode[],
  currentNodeId: string | null,
  selectedNodeId: string | null,
  hoveredNodeId: string | null = null,
): EngineRFNode[] {
  return nodes.map((n) => {
    if (n.type === "engineFlow") {
      const data = n.data as EngineFlowData;
      const current = currentNodeId === n.id;
      const selected = selectedNodeId === n.id;
      const hovered = hoveredNodeId === n.id;
      if (data.current === current && data.selected === selected && data.hovered === hovered) return n;
      return { ...n, data: { ...data, current, selected, hovered } };
    }
    if (n.type === "engineGroup") {
      const data = n.data as EngineGroupData;
      const selected = selectedNodeId === n.id;
      const hovered = hoveredNodeId === n.id;
      if (data.selected === selected && data.hovered === hovered) return n;
      return { ...n, data: { ...data, selected, hovered } };
    }
    return n;
  });
}

/**
 * Derive a `loop`'s READABLE exit condition from its `until` config (finding #39). Mirrors nyx-core's
 * `engine::Condition` shape (`{var, eq|ne|truthy}`, or `{always}`): a named blackboard `var` compared
 * to a value. Returns e.g. `phase_status = complete` / `gate_green ≠ true` / `is set` — or `null` when
 * `until` is absent or has no describable test, so the header falls back to a bare `↺` without crashing.
 */
export function describeLoopCondition(config: string): string | null {
  return describeCondition(parseNodeConfig(config).until);
}

/** Format a nyx `engine::Condition` (`{var, eq|ne|truthy}` or `{always}`) as a short readable string —
 *  e.g. `phase_status = complete` / `gate_green ≠ true` / `always`. `null` when there is no describable
 *  test. Shared by the loop `until` header and the selector BRANCH edge labels (N18(b)). */
export function describeCondition(cond: unknown): string | null {
  if (!cond || typeof cond !== "object") return null;
  const c = cond as Record<string, unknown>;
  if (c.always === true) return "always";
  const varName = typeof c.var === "string" ? c.var : null;
  if (!varName) return null;
  if ("eq" in c) return `${varName} = ${formatCondValue(c.eq)}`;
  if ("ne" in c) return `${varName} ≠ ${formatCondValue(c.ne)}`;
  if (typeof c.truthy === "boolean") return c.truthy ? `${varName} is set` : `${varName} is unset`;
  return varName; // a bare `var` — truthiness/presence test.
}

/** What a branch whose `cond` is PRESENT but names neither a `var` nor `always: true` is labelled —
 *  `{}`, `{"eq": "yes"}`, `{"always": false}`. nyx-core deserializes all of these without error
 *  (`Condition`'s fields are all optional) and `Condition::matches` then returns `false` for every
 *  board: such a branch can NEVER fire. It is the exact opposite of the default arm, and its own
 *  `Condition::describe()` says so — "nothing (neither a var nor 'always')". It is worth a WORD
 *  rather than a blank, because a blank is what made it readable as the default. */
export const NEVER_MATCHES_LABEL = "never (no var, no always)";

/** The readable condition under which a SELECTOR routes to the child at `childIndex` (its position
 *  among the selector's ordered children). Mirrors nyx-core's `SelectorBranch` (`{cond, child}`, where
 *  an absent `child` defaults to the branch's own position).
 *
 *  SEVERAL branches may target the SAME child — nyx-core allows it (`pick_branch` takes the first that
 *  matches), and `/nspec`'s `route ready` shipped exactly that shape (`ready_ok = yes` and
 *  `ready_ok = go` both selecting the one `transition`) until revision 22 gave `go` a branch of its
 *  own. ALL of their conditions are returned, as one
 *  DISJUNCTION in branch order: `ready_ok = yes OR ready_ok = go`. Showing only the first would claim
 *  the other values fall through to the `ELSE` arm — the exact opposite of what the graph does, on the
 *  one surface a human reads instead of the config. One branch → its condition VERBATIM
 *  (`spec_status = ready` / `always`), so a 1-branch-per-child selector reads exactly as before.
 *
 *  THE THREE SHAPES ARE READ FROM THE BRANCH, NOT FROM A RENDERED STRING (finding
 *  `01M0AE30KM8MC6PD7DWRSM06PW`). `describeCondition` answers `null` for TWO shapes nyx-core holds
 *  apart, and collapsing on that answer merged them:
 *    1. NO `cond` key at all → `cond: None`, which `pick_branch` matches UNCONDITIONALLY: the default
 *       arm. The child is reachable whatever the others test, so the disjunction collapses onto it and
 *       this returns `null` (the inspector's `ELSE`).
 *    2. `always: true` → same reachability, same collapse. Tested on the SHAPE, not on the rendered
 *       string, so a blackboard var literally named `always` (`{"var": "always"}` — a real presence
 *       test, rendered `always` too) no longer COLLAPSES the disjunction and no longer erases what its
 *       siblings contribute. Its rendered string stays ambiguous for a caller that compares against
 *       `"always"` (the inspector's `isDefault` does): that half is the annexe note of the same
 *       finding, pre-existing and left as it stands.
 *    3. `cond` PRESENT but with neither → matches NEVER ([`NEVER_MATCHES_LABEL`]). It contributes
 *       nothing to the disjunction and, above all, DESTROYS nothing: on
 *       `[{cond:{var:"x",eq:1},child:0},{cond:{},child:0}]` the answer is `x = 1`, not `ELSE`.
 *       The old short-circuit returned `null` there — painting an arm reached only when `x == 1` as
 *       the one reached always, which is the very reversal this function exists to prevent.
 *  No selector seeded in nyx-core carries shape 3 today, but nothing REFUSES it either
 *  (`validate_node_config` has no `selector` arm and never looks inside a branch), so it is authorable.
 *
 *  `null` when no branch targets that child. Powers the branch-edge labels (N18(b)) and the
 *  inspector's branch list. */
export function describeBranchCondition(selectorConfig: string, childIndex: number): string | null {
  const branches = parseNodeConfig(selectorConfig).branches;
  if (!Array.isArray(branches)) return null;
  const conditions: string[] = [];
  let sawNeverMatching = false;
  for (let i = 0; i < branches.length; i += 1) {
    const b = branches[i];
    if (!b || typeof b !== "object") continue;
    const rec = b as Record<string, unknown>;
    const target = typeof rec.child === "number" ? rec.child : i;
    if (target !== childIndex) continue;
    const cond = rec.cond;
    // (1) The DEFAULT arm — nothing to OR it with.
    if (cond === undefined || cond === null) return null;
    // (2) `always: true` — likewise, and read from the shape so a var named `always` is not it.
    if (typeof cond === "object" && (cond as Record<string, unknown>).always === true) {
      return "always";
    }
    const described = describeCondition(cond);
    // (3) Present, undescribable ⇒ unreachable through this branch. Skip it, keep what we have.
    if (described === null) {
      sawNeverMatching = true;
      continue;
    }
    if (!conditions.includes(described)) conditions.push(described); // the same test twice is noise.
  }
  if (conditions.length > 0) return conditions.join(" OR ");
  return sawNeverMatching ? NEVER_MATCHES_LABEL : null;
}

/** Render a `Condition` comparison value plainly (a string without quotes, else its JSON form). */
function formatCondValue(value: unknown): string {
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return JSON.stringify(value);
}

/** Parse a node's `config` JSON-object string defensively (a malformed blob → `{}`). Exported so the
 *  page + the type-adapted inspector (finding #32) read a node's config through the SAME decoder. */
export function parseNodeConfig(config: string): Record<string, unknown> {
  try {
    const parsed = JSON.parse(config) as unknown;
    return parsed && typeof parsed === "object" ? (parsed as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}
