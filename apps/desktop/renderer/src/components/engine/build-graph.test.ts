import { describe, expect, it } from "vitest";

import type { EngineNode } from "@/bridge";

import {
  applyEngineOverlay,
  buildFlowGraph,
  describeLoopCondition,
  graphStructureKey,
  START_ID,
  TERMINAL_ID,
  type EngineFlowData,
  type EngineGroupData,
} from "./build-graph";

/** Build an `EngineNode` row with sane defaults (mirrors the sidecar `engine_workflow_graph` shape). */
function mk(
  id: string,
  parentId: string | null,
  orderIndex: number,
  type: string,
  label: string,
  config: Record<string, unknown> = {},
): EngineNode {
  return {
    id,
    workflow_id: "wf",
    parent_id: parentId,
    order_index: orderIndex,
    type_: type,
    label,
    config: JSON.stringify(config),
    created_at: 0,
    updated_at: 0,
  };
}

/**
 * A fixture mirroring the seeded ndev tree PLUS a nested `sequence` and a `selector`, so the graph
 * exercises every clause: the 5 leaf flow types (spawn/gate/judge/selector/transition), BOTH group
 * types (loop + sequence), and the loop's reboucle:
 *
 *   sequence "/ndev" (root)
 *   ├─ loop "phases"
 *   │  ├─ sequence "body"
 *   │  │  ├─ spawn_subagent "coder"      {agent_id: coder}
 *   │  │  └─ gate_check "checks"         {command_ids: [typecheck]}
 *   │  ├─ judge "phase review"
 *   │  └─ selector "phase green?"
 *   ├─ spawn_subagent "auditor"          {agent_id: auditor}
 *   ├─ judge "audit review"
 *   └─ transition "→ review"             {target_state: in_review}
 */
function ndevFixture(): EngineNode[] {
  return [
    mk("root", null, 0, "sequence", "/ndev"),
    mk("loop", "root", 0, "loop", "phases", { until: { var: "phase_status", eq: "complete" } }),
    mk("body", "loop", 0, "sequence", "body"),
    mk("coder", "body", 0, "spawn_subagent", "coder", { agent_id: "coder" }),
    mk("gate", "body", 1, "gate_check", "checks", { command_ids: ["typecheck"], blocking: true }),
    mk("judgePhase", "loop", 1, "judge", "phase review"),
    mk("selector", "loop", 2, "selector", "phase green?"),
    mk("auditor", "root", 1, "spawn_subagent", "auditor", { agent_id: "auditor" }),
    mk("judgeAudit", "root", 2, "judge", "audit review"),
    mk("transition", "root", 3, "transition", "→ review", { target_state: "in_review" }),
  ];
}

/**
 * A `selector` with THREE real branches, framed by a leaf before and a leaf after — the topology
 * finding #33 is about. Expected flow: `query → selector`, then the selector FANS OUT to each branch
 * (`selector → cancel|hold|ok`) and the branches CONVERGE on the selector's successor
 * (`cancel|hold|ok → done`). Branches must NOT chain into each other.
 *
 *   sequence "/route" (root)
 *   ├─ query "status"
 *   ├─ selector "route"   branches: review=cancel→0, review=hold→1, always→2
 *   │  ├─ transition "cancel"   (child 0)
 *   │  ├─ note "hold"           (child 1)
 *   │  └─ note "ok"             (child 2)
 *   └─ produce "done"
 */
function selectorFixture(): EngineNode[] {
  const branches = {
    branches: [
      { cond: { var: "review", eq: "cancel" }, child: 0 },
      { cond: { var: "review", eq: "hold" }, child: 1 },
      { cond: { always: true }, child: 2 },
    ],
  };
  return [
    mk("root", null, 0, "sequence", "/route"),
    mk("query", "root", 0, "query", "status"),
    mk("selector", "root", 1, "selector", "route", branches),
    mk("cancel", "selector", 0, "transition", "cancel"),
    mk("hold", "selector", 1, "note", "hold"),
    mk("ok", "selector", 2, "note", "ok"),
    mk("done", "root", 2, "produce", "done"),
  ];
}

/**
 * The shared-child selector shape: THREE branches for TWO children — `ready_ok = yes` AND
 * `ready_ok = go` both select child 0 (nyx-core's `pick_branch` takes the first that matches, and
 * nothing requires a branch per child). It is what `/nspec`'s `route ready` shipped until revision 22
 * split `go` onto its own branch; the renderer must keep reading it, since nyx-core still allows it
 * and any fork may use it. The graph draws ONE edge per child, so that edge has to carry
 * BOTH conditions: labelling it `ready_ok = yes` alone says `go` falls to the `ELSE` and leaves the
 * spec a draft — the opposite of what the run does.
 *
 *   sequence "/route" (root)
 *   └─ selector "route ready"   branches: ready_ok=yes→0, ready_ok=go→0, always→1
 *      ├─ transition "ready"        (child 0)
 *      └─ note "left as draft"      (child 1)
 */
function sharedChildSelectorFixture(): EngineNode[] {
  const branches = {
    branches: [
      { cond: { var: "ready_ok", eq: "yes" }, child: 0 },
      { cond: { var: "ready_ok", eq: "go" }, child: 0 },
      { cond: { always: true }, child: 1 },
    ],
  };
  return [
    mk("root", null, 0, "sequence", "/route"),
    mk("selector", "root", 0, "selector", "route ready", branches),
    mk("ready", "selector", 0, "transition", "ready"),
    mk("draft", "selector", 1, "note", "left as draft"),
  ];
}

const flowData = (data: unknown) => data as EngineFlowData;
const groupData = (data: unknown) => data as EngineGroupData;

describe("buildFlowGraph", () => {
  it("returns an empty graph for no nodes", async () => {
    expect(await buildFlowGraph([], null)).toEqual({ nodes: [], edges: [] });
  });

  it("renders a custom FLOW node for each leaf/decision type (spawn/gate/judge/selector/transition)", async () => {
    const { nodes } = await buildFlowGraph(ndevFixture(), "root");
    const byId = new Map(nodes.map((n) => [n.id, n]));

    for (const [id, type] of [
      ["coder", "spawn_subagent"],
      ["gate", "gate_check"],
      ["judgePhase", "judge"],
      ["selector", "selector"],
      ["transition", "transition"],
      ["auditor", "spawn_subagent"],
    ] as const) {
      const node = byId.get(id)!;
      expect(node.type).toBe("engineFlow");
      expect(flowData(node.data).nodeType).toBe(type);
    }
  });

  it("wraps loop AND sequence composites in GROUP nodes", async () => {
    const { nodes } = await buildFlowGraph(ndevFixture(), "root");
    const groups = nodes.filter((n) => n.type === "engineGroup");
    const kinds = groups.map((g) => groupData(g.data).nodeType);
    expect(kinds).toContain("loop");
    expect(kinds).toContain("sequence");
    // root + body are sequences, phases is a loop → three group containers.
    expect(groups).toHaveLength(3);
  });

  it("nests group containers via React Flow parentId (coder → body → loop → root)", async () => {
    const { nodes } = await buildFlowGraph(ndevFixture(), "root");
    const byId = new Map(nodes.map((n) => [n.id, n]));
    expect(byId.get("coder")!.parentId).toBe("body");
    expect(byId.get("body")!.parentId).toBe("loop");
    expect(byId.get("loop")!.parentId).toBe("root");
    expect(byId.get("root")!.parentId).toBeUndefined();
  });

  // finding #34 — the CRUX: ELK lays the graph out as a COMPOUND graph, so each group box CONTAINS its
  // children (no more overlap / edges crossing through boxes, which the old post-hoc box derivation
  // let happen). ELK positions a child relative to its parent within the container's padding, so every
  // node with a `parentId` fits fully inside that parent's laid-out box. Pin it so a regression is
  // caught. A parent must also always be emitted BEFORE its children (React Flow requires it).
  it("respects the hierarchy: every group box CONTAINS its children (ELK compound layout)", async () => {
    const { nodes } = await buildFlowGraph(ndevFixture(), "root");
    const byId = new Map(nodes.map((n) => [n.id, n]));
    const index = new Map(nodes.map((n, i) => [n.id, i]));
    // A node's footprint: a group carries its box in `style`, a leaf is the fixed 254×70.
    const dims = (n: (typeof nodes)[number]) => {
      const w = n.style?.width;
      const h = n.style?.height;
      return typeof w === "number" && typeof h === "number" ? { w, h } : { w: 254, h: 70 };
    };
    const EPS = 0.5; // ELK returns floats; tolerate sub-pixel rounding.
    let checked = 0;
    for (const node of nodes) {
      if (node.parentId == null) continue;
      const parent = byId.get(node.parentId)!;
      // The parent container is emitted before the child (React Flow ordering contract).
      expect(index.get(node.parentId)!).toBeLessThan(index.get(node.id)!);
      const parentDims = dims(parent);
      const childDims = dims(node);
      // Child position is relative to the parent container — it sits fully inside the box.
      expect(node.position.x).toBeGreaterThanOrEqual(-EPS);
      expect(node.position.y).toBeGreaterThanOrEqual(-EPS);
      expect(node.position.x + childDims.w).toBeLessThanOrEqual(parentDims.w + EPS);
      expect(node.position.y + childDims.h).toBeLessThanOrEqual(parentDims.h + EPS);
      checked += 1;
    }
    // coder+gate (in body), body+judgePhase+selector (in loop), loop+auditor+judgeAudit+transition
    // (in root) — every non-top-level node is containment-checked.
    expect(checked).toBeGreaterThanOrEqual(9);
  });

  it("brackets the run with synthetic start + terminal PILLS", async () => {
    const { nodes } = await buildFlowGraph(ndevFixture(), "root");
    const start = nodes.find((n) => n.id === START_ID)!;
    const terminal = nodes.find((n) => n.id === TERMINAL_ID)!;
    expect(start.type).toBe("enginePill");
    expect(terminal.type).toBe("enginePill");
  });

  // finding #39 — the loop's reboucle is shown on its BOX header (↺ + condition), never a floating
  // back-edge that overflowed the container. So NO external reboucle edge is emitted anymore.
  it("emits NO external reboucle back-edge (loop-back lives on the box header)", async () => {
    const { edges } = await buildFlowGraph(ndevFixture(), "root");
    // Every edge is the neutral sequential FLOW; the `reboucle` variant no longer exists.
    expect(edges.every((e) => e.data?.variant === "flow")).toBe(true);
    // No edge crosses the loop's last body leaf back to its first (the old selector → coder arc).
    expect(edges.some((e) => e.source === "selector" && e.target === "coder")).toBe(false);
    // The old floating reboucle edge id (`reboucle-<loop>`) is not emitted.
    expect(edges.some((e) => e.id.startsWith("reboucle-"))).toBe(false);
  });

  // finding #39 — the loop GROUP carries its readable `until` exit condition (its `↺` header
  // indicator), derived from the loop config; a sequence group has none.
  it("carries the loop's until exit condition on the loop group (loopBack), null for a sequence", async () => {
    const { nodes } = await buildFlowGraph(ndevFixture(), "root");
    const byId = new Map(nodes.map((n) => [n.id, n]));
    expect(groupData(byId.get("loop")!.data).loopBack).toBe("phase_status = complete");
    expect(groupData(byId.get("root")!.data).loopBack).toBeNull();
    expect(groupData(byId.get("body")!.data).loopBack).toBeNull();
  });

  it("wires the sequential flow start → … → terminal", async () => {
    const { edges } = await buildFlowGraph(ndevFixture(), "root");
    const flow = edges.filter((e) => e.data?.variant === "flow");
    const pairs = new Set(flow.map((e) => `${e.source}->${e.target}`));
    expect(pairs.has(`${START_ID}->coder`)).toBe(true);
    expect(pairs.has("coder->gate")).toBe(true);
    expect(pairs.has(`transition->${TERMINAL_ID}`)).toBe(true);
  });

  // finding #33 — a selector FANS OUT to each branch (spread), instead of chaining the alternatives
  // into one vertical column. Each fan-out edge carries its branch condition (N18(b)).
  it("fans a selector OUT to each branch, not chained, each edge labelled with its condition", async () => {
    const { edges } = await buildFlowGraph(selectorFixture(), "root");
    const pairs = new Set(edges.map((e) => `${e.source}->${e.target}`));
    // Entering the selector, then one edge per branch (fan-out).
    expect(pairs.has("query->selector")).toBe(true);
    expect(pairs.has("selector->cancel")).toBe(true);
    expect(pairs.has("selector->hold")).toBe(true);
    expect(pairs.has("selector->ok")).toBe(true);
    // The branches are NOT chained into each other (the bug: alternatives read as a sequence).
    expect(pairs.has("cancel->hold")).toBe(false);
    expect(pairs.has("hold->ok")).toBe(false);
    // Each fan-out edge carries the branch's readable condition.
    const cond = (source: string, target: string) =>
      edges.find((e) => e.source === source && e.target === target)?.data?.branchCondition;
    expect(cond("selector", "cancel")).toBe("review = cancel");
    expect(cond("selector", "hold")).toBe("review = hold");
    expect(cond("selector", "ok")).toBe("always");
  });

  // N branches → 1 child: the fan-out edge is per CHILD, so its label must name EVERY condition that
  // reaches that child. Only the first one meant `go` read as falling into the `ELSE`.
  it("labels the branch edge with EVERY condition routing to that child, not only the first", async () => {
    const { edges } = await buildFlowGraph(sharedChildSelectorFixture(), "root");
    const pairs = new Set(edges.map((e) => `${e.source}->${e.target}`));
    // Still ONE edge per child — the shared child is not drawn twice.
    expect(pairs.has("selector->ready")).toBe(true);
    expect(pairs.has("selector->draft")).toBe(true);
    expect(edges.filter((e) => e.source === "selector" && e.target === "ready")).toHaveLength(1);
    const cond = (target: string) =>
      edges.find((e) => e.source === "selector" && e.target === target)?.data?.branchCondition;
    expect(cond("ready")).toBe("ready_ok = yes OR ready_ok = go");
    expect(cond("draft")).toBe("always");
  });

  // finding #33 — every branch CONVERGES on the selector's next sibling (the join), so the flow
  // re-merges after the fork instead of running off the last branch only.
  it("converges every selector branch onto the selector's successor (the join)", async () => {
    const { edges } = await buildFlowGraph(selectorFixture(), "root");
    const pairs = new Set(edges.map((e) => `${e.source}->${e.target}`));
    for (const branchTail of ["cancel", "hold", "ok"]) {
      expect(pairs.has(`${branchTail}->done`)).toBe(true);
    }
    // The join edges are plain flow (no branch condition — that lives on the fan-out side).
    const join = edges.find((e) => e.source === "cancel" && e.target === "done");
    expect(join?.data?.branchCondition).toBeUndefined();
    // Every edge id is unique (React Flow requires it, even with fan-in on `done`).
    expect(new Set(edges.map((e) => e.id)).size).toBe(edges.length);
  });

  it("HIGHLIGHTS the runtime current node (data.current)", async () => {
    const { nodes } = await buildFlowGraph(ndevFixture(), "root", "coder");
    const byId = new Map(nodes.map((n) => [n.id, n]));
    expect(flowData(byId.get("coder")!.data).current).toBe(true);
    expect(flowData(byId.get("gate")!.data).current).toBe(false);
  });

  // v2.8 Phase 7 GATE (clause 4 — "visualiser runtime fidèle", the renderer-highlight half): the
  // highlight FAITHFULLY tracks whatever `currentNodeId` the run reports (the sidecar
  // `engine_run_list` returns nyx_next's persisted cursor verbatim — proven in the sidecar
  // `engine_run_list_reflects_a_live_nyx_next_run` test). As the run walks its nodes, exactly the
  // reported node lights up and no other — so the visualiser never diverges from the real cursor.
  it("faithfully tracks the run cursor: exactly the reported current node is highlighted", async () => {
    // The successive cursors a real ndev run parks on (spawn → gate → judge → … → transition).
    for (const cursor of ["coder", "gate", "judgePhase", "auditor", "judgeAudit", "transition"]) {
      const { nodes } = await buildFlowGraph(ndevFixture(), "root", cursor);
      const highlighted = nodes.filter((n) => n.type === "engineFlow" && flowData(n.data).current);
      expect(highlighted).toHaveLength(1);
      expect(highlighted[0]!.id).toBe(cursor);
    }
  });

  it("highlights NOTHING for a static view (no run cursor) or an unknown cursor", async () => {
    for (const cursor of [null, "not-a-real-node"]) {
      const { nodes } = await buildFlowGraph(ndevFixture(), "root", cursor);
      const highlighted = nodes.filter((n) => n.type === "engineFlow" && flowData(n.data).current);
      expect(highlighted).toHaveLength(0);
    }
  });

  // finding #33 — the SELECTED node (inspector open) is flagged in node data (flow + group), so the
  // canvas can paint a distinct selected state.
  it("flags the selected node in flow AND group data (data.selected)", async () => {
    const { nodes } = await buildFlowGraph(ndevFixture(), "root", null, "coder");
    const byId = new Map(nodes.map((n) => [n.id, n]));
    expect(flowData(byId.get("coder")!.data).selected).toBe(true);
    expect(flowData(byId.get("gate")!.data).selected).toBe(false);

    const { nodes: withGroup } = await buildFlowGraph(ndevFixture(), "root", null, "loop");
    const loop = withGroup.find((n) => n.id === "loop")!;
    expect(groupData(loop.data).selected).toBe(true);
  });

  it("carries the spawn agent_id and gate command_ids into node data (marker sources)", async () => {
    const { nodes } = await buildFlowGraph(ndevFixture(), "root");
    const byId = new Map(nodes.map((n) => [n.id, n]));
    expect(flowData(byId.get("coder")!.data).agentId).toBe("coder");
    expect(flowData(byId.get("gate")!.data).commandIds).toEqual(["typecheck"]);
  });

  it("lays every node out with finite ELK coordinates", async () => {
    const { nodes } = await buildFlowGraph(ndevFixture(), "root");
    for (const node of nodes) {
      expect(Number.isFinite(node.position.x)).toBe(true);
      expect(Number.isFinite(node.position.y)).toBe(true);
    }
  });
});

// finding #35 — the graph↔children CROSS-HIGHLIGHT re-flags `data.hovered` on the laid-out nodes
// (flow AND group) WITHOUT a relayout, exactly like the current/selected overlay. This is the shared
// mechanism BOTH hover directions ride on (list→graph and graph→list).
describe("applyEngineOverlay — hovered", () => {
  it("flags exactly the hovered flow node, clearing the others (cheap re-map, no relayout)", async () => {
    const { nodes } = await buildFlowGraph(ndevFixture(), "root");
    const overlaid = applyEngineOverlay(nodes, null, null, "coder");
    const byId = new Map(overlaid.map((n) => [n.id, n]));
    expect(flowData(byId.get("coder")!.data).hovered).toBe(true);
    expect(flowData(byId.get("gate")!.data).hovered).toBe(false);
    // Clearing the hover unflags it again.
    const cleared = applyEngineOverlay(overlaid, null, null, null);
    expect(flowData(new Map(cleared.map((n) => [n.id, n])).get("coder")!.data).hovered).toBe(false);
  });

  it("flags a hovered GROUP node too (a composite child hovered in the list)", async () => {
    const { nodes } = await buildFlowGraph(ndevFixture(), "root");
    const overlaid = applyEngineOverlay(nodes, null, null, "loop");
    const loop = overlaid.find((n) => n.id === "loop")!;
    expect(groupData(loop.data).hovered).toBe(true);
    // hover is independent of selection/current — both stay off here.
    expect(groupData(loop.data).selected).toBe(false);
  });
});

// finding #39 — the readable loop-back condition derived from a loop's `until` config (mirrors the
// nyx-core `engine::Condition` shape), used as the loop box's `↺` header indicator.
describe("describeLoopCondition", () => {
  const cfg = (until: unknown) => JSON.stringify({ until });

  it("renders an `eq` test as `var = value` (the seeded ndev loop)", () => {
    expect(describeLoopCondition(cfg({ var: "phase_status", eq: "complete" }))).toBe(
      "phase_status = complete",
    );
  });

  it("renders a `ne` test as `var ≠ value`", () => {
    expect(describeLoopCondition(cfg({ var: "gate_green", ne: true }))).toBe("gate_green ≠ true");
  });

  it("renders truthy / falsy and a bare var presence test", () => {
    expect(describeLoopCondition(cfg({ var: "done", truthy: true }))).toBe("done is set");
    expect(describeLoopCondition(cfg({ var: "done", truthy: false }))).toBe("done is unset");
    expect(describeLoopCondition(cfg({ var: "verdict" }))).toBe("verdict");
    expect(describeLoopCondition(cfg({ always: true }))).toBe("always");
  });

  it("returns null (no crash) for an absent / undescribable until", () => {
    expect(describeLoopCondition(JSON.stringify({}))).toBeNull();
    expect(describeLoopCondition(JSON.stringify({ until: {} }))).toBeNull();
    expect(describeLoopCondition("not json")).toBeNull();
    expect(describeLoopCondition(JSON.stringify({ until: "complete" }))).toBeNull();
  });
});

// finding #37(a) — the async ELK layout is keyed on this STABLE signature, not the graph object's
// identity, so a live re-pull of an unchanged graph does NOT re-lay-out (the flicker / double-position).
describe("graphStructureKey", () => {
  it("is IDENTICAL for two structurally-equal graphs built as separate objects (a live re-pull)", () => {
    // Two independent builds of the same fixture = a fresh object with identical structure.
    expect(graphStructureKey(ndevFixture(), "root")).toBe(graphStructureKey(ndevFixture(), "root"));
  });

  it("CHANGES on any real shape/content edit (so a genuine change still re-lays-out)", () => {
    const base = graphStructureKey(ndevFixture(), "root");
    // add a node
    expect(
      graphStructureKey([...ndevFixture(), mk("extra", "root", 9, "note", "extra")], "root"),
    ).not.toBe(base);
    // relabel a node
    expect(
      graphStructureKey(
        ndevFixture().map((n) => (n.id === "coder" ? { ...n, label: "CODER!" } : n)),
        "root",
      ),
    ).not.toBe(base);
    // reorder a sibling
    expect(
      graphStructureKey(
        ndevFixture().map((n) => (n.id === "auditor" ? { ...n, order_index: 99 } : n)),
        "root",
      ),
    ).not.toBe(base);
    // reparent
    expect(
      graphStructureKey(
        ndevFixture().map((n) => (n.id === "judgeAudit" ? { ...n, parent_id: "loop" } : n)),
        "root",
      ),
    ).not.toBe(base);
    // change the config
    expect(
      graphStructureKey(
        ndevFixture().map((n) => (n.id === "gate" ? { ...n, config: "{}" } : n)),
        "root",
      ),
    ).not.toBe(base);
    // change the root
    expect(graphStructureKey(ndevFixture(), "loop")).not.toBe(base);
  });
});

// The lot-2 node types must carry their OWN identity in the graph: falling back to `note` drew the
// node that opens a child run as an inert marker (spotted live in nspec's proto branch).
describe("lot-2 node types render as themselves", () => {
  it("a sub_workflow node is not the note fallback", async () => {
    const nodes = [
      mk("root", null, 0, "sequence", "/nspec"),
      mk("proto", "root", 0, "sub_workflow", "prototyper", { scope: "prototyper" }),
      mk("tt", "root", 1, "transition_task", "mark done", { transition: "done" }),
    ];
    const { nodes: rf } = await buildFlowGraph(nodes, "root");
    const byId = new Map(rf.map((n) => [n.id, n]));
    expect(flowData(byId.get("proto")!.data).nodeType).toBe("sub_workflow");
    expect(flowData(byId.get("tt")!.data).nodeType).toBe("transition_task");
  });
});
