import { describe, expect, it } from "vitest";

import { taskStatusMeta } from "../specs/spec-meta";
import type { SpecDetail, Task } from "../specs/types";
import {
  BAR_TRACK,
  barZonesOf,
  currentPhaseIndex,
  POINT_TONE,
  RAIL_BAR_HEIGHT_PX,
  RAIL_DOT_SIZE_PX,
  RAIL_ITEM_WIDTH_PX,
  railItemMorph,
  railRollup,
  tasksOfPhase,
  type BarZoneKey,
  type RailItem,
  type RailRollup,
} from "./overlay-status";

/** A minimal `tasks` row — only `phase`/`status` matter to the rail roll-up (id is irrelevant). */
function task(phase: number | null, status: string): Task {
  return {
    id: `t-${phase}-${status}`,
    project_id: "p",
    spec_id: "s",
    parent_task_id: null,
    kind: "spec",
    executor: "agent",
    type_: "",
    status,
    priority: "med",
    severity: null,
    title: "task",
    description: null,
    acceptance_criteria: null,
    verification_command: null,
    phase,
    position: 0,
    blocked_reason: null,
    skip_reason: null,
    created_at: 0,
    updated_at: 0,
    completed_at: null,
  };
}

/** A minimal `spec_detail` — the roll-up reads only `spec.phases` (JSON) + `tasks`. */
function detail(phasesJson: string, tasks: Task[]): SpecDetail {
  return {
    spec: {
      id: "s",
      project_id: "p",
      workspace_id: null,
      title: "Spec",
      slug: null,
      number: 1,
      priority: "med",
      status: "in_progress",
      mode: "stepped",
      phases: phasesJson,
      current_revision_number: 1,
      position: 0,
      created_at: 0,
      updated_at: 0,
      completed_at: null,
      archived_at: null,
    },
    revision: {
      id: "r",
      spec_id: "s",
      revision_number: 1,
      revision_status: "active",
      title: "Spec",
      problem: "",
      solution: "",
      implementation_decisions: "",
      testing_decisions: "",
      user_stories: "",
      out_of_scope: "",
      change_reason: null,
      created_at: 0,
    },
    tasks,
    dependencies: [],
    workspace_context: null,
  };
}

const THREE_PHASES = JSON.stringify([
  { number: 1, name: "P1" },
  { number: 2, name: "P2" },
  { number: 3, name: "P3" },
]);

/** The ordered `kind` sequence — the shape the rail renders top→bottom. */
function kinds(rollup: RailRollup): string[] {
  return rollup.items.map((it) => it.kind);
}
/** The single `bar` item's %, or `undefined` when there is no bar (all phases done). */
function barPct(rollup: RailRollup): number | undefined {
  return rollup.items.find((it) => it.kind === "bar")?.pct;
}
/** The `bar` item itself, or `undefined` when there is no bar (all phases done). */
function bar(rollup: RailRollup): RailItem | undefined {
  return rollup.items.find((it) => it.kind === "bar");
}
/** The height % of a given zone on a bar item, or 0 when that zone is absent (no segment). */
function zoneH(item: RailItem | undefined, key: BarZoneKey): number {
  return item?.segments?.find((s) => s.key === key)?.heightPct ?? 0;
}
/** The tone class of a given zone on a bar item, or `undefined` when the zone is absent. */
function zoneTone(item: RailItem | undefined, key: BarZoneKey): string | undefined {
  return item?.segments?.find((s) => s.key === key)?.tone;
}
/** The ordered zone keys of a bar item's segments (stack order top→bottom). */
function zoneKeys(item: RailItem | undefined): BarZoneKey[] {
  return (item?.segments ?? []).map((s) => s.key);
}

const FIVE_PHASES = JSON.stringify([
  { number: 1, name: "P1" },
  { number: 2, name: "P2" },
  { number: 3, name: "P3" },
  { number: 4, name: "P4" },
  { number: 5, name: "P5" },
]);
const ONE_PHASE = JSON.stringify([{ number: 1, name: "P1" }]);

describe("railRollup — ordered sequence (bar INLINE at the current phase)", () => {
  it("CURRENT PHASE = the first phase NOT entirely done; the bar tracks THAT phase's % only, inline", () => {
    // Phase 1 done, phase 2 half-done (the current phase), phase 3 untouched.
    const d = detail(THREE_PHASES, [
      task(1, "done"),
      task(1, "done"),
      task(2, "done"),
      task(2, "todo"),
      task(3, "todo"),
    ]);
    const rollup: RailRollup = railRollup(d);
    // ORDERED: phase 1 = done dot, phase 2 (current) = the BAR inline, phase 3 = remaining dot.
    expect(kinds(rollup)).toEqual(["done", "bar", "remaining"]);
    // Total items = number of phases (the bar counts as the current phase).
    expect(rollup.items).toHaveLength(3);
    // The bar follows the CURRENT phase (phase 2 = 1 done / 2 active = 50%), NOT the global % (3/5 = 60%).
    expect(barPct(rollup)).toBe(50);
  });

  it("bar is INLINE at the current phase's index — 5 phases / phase 3 current ⇒ [dot,dot,BAR,dot,dot]", () => {
    // Phases 1 & 2 done, phase 3 in progress (current), phases 4 & 5 untouched.
    const d = detail(FIVE_PHASES, [
      task(1, "done"),
      task(2, "done"),
      task(3, "done"),
      task(3, "todo"),
      task(4, "todo"),
      task(5, "todo"),
    ]);
    const rollup = railRollup(d);
    expect(kinds(rollup)).toEqual(["done", "done", "bar", "remaining", "remaining"]);
    expect(rollup.items).toHaveLength(5); // total = nb phases
    expect(barPct(rollup)).toBe(50); // phase 3 = 1 done / 2 active
  });

  it("current phase = the FIRST phase ⇒ the BAR is the FIRST item (top)", () => {
    // Phase 1 in progress (current), phase 2 untouched ⇒ [BAR, dot].
    const d = detail(
      JSON.stringify([
        { number: 1, name: "P1" },
        { number: 2, name: "P2" },
      ]),
      [task(1, "done"), task(1, "todo"), task(2, "todo")],
    );
    const rollup = railRollup(d);
    expect(kinds(rollup)).toEqual(["bar", "remaining"]);
    expect(barPct(rollup)).toBe(50); // phase 1 = 1 done / 2 active
  });

  it("ONE phase (current) ⇒ a single BAR item", () => {
    const d = detail(ONE_PHASE, [task(1, "done"), task(1, "todo")]);
    const rollup = railRollup(d);
    expect(kinds(rollup)).toEqual(["bar"]);
    expect(barPct(rollup)).toBe(50);
  });

  it("EVERY phase done ⇒ a done dot per phase and NO bar (no current phase)", () => {
    const d = detail(THREE_PHASES, [task(1, "done"), task(2, "done"), task(3, "done")]);
    const rollup = railRollup(d);
    expect(kinds(rollup)).toEqual(["done", "done", "done"]);
    // No current phase ⇒ NO bar item at all.
    expect(rollup.items.some((it) => it.kind === "bar")).toBe(false);
    expect(barPct(rollup)).toBeUndefined();
  });

  it("FLAT spec (no phases) ⇒ a SINGLE bar = the GLOBAL task %", () => {
    const d = detail("[]", [
      task(null, "done"),
      task(null, "todo"),
      task(null, "todo"),
      task(null, "todo"),
    ]);
    const rollup = railRollup(d);
    expect(kinds(rollup)).toEqual(["bar"]);
    expect(barPct(rollup)).toBe(25); // 1 done / 4 active
  });

  it("NO tasks ⇒ current = phase 1, so the BAR is FIRST at 0% (never NaN), then remaining dots", () => {
    const d = detail(THREE_PHASES, []);
    const rollup = railRollup(d);
    // Current = phase 1 (first not-done) ⇒ bar in first position; phases 2 & 3 remain.
    expect(kinds(rollup)).toEqual(["bar", "remaining", "remaining"]);
    expect(rollup.items).toHaveLength(3);
    expect(barPct(rollup)).toBe(0);
    expect(Number.isNaN(barPct(rollup))).toBe(false);
  });

  it("FLAT spec with NO tasks ⇒ a single empty bar (0%, no divide-by-zero)", () => {
    const rollup = railRollup(detail("[]", []));
    expect(kinds(rollup)).toEqual(["bar"]);
    expect(barPct(rollup)).toBe(0);
  });

  it("treats all-skipped (cancelled) phases as done and puts the bar at the current phase", () => {
    // Phase 1 all skipped ⇒ reads as resolved/done; phase 2 is the current (todo) phase.
    const d = detail(THREE_PHASES, [task(1, "cancelled"), task(2, "todo"), task(3, "todo")]);
    const rollup = railRollup(d);
    expect(kinds(rollup)).toEqual(["done", "bar", "remaining"]);
    expect(barPct(rollup)).toBe(0); // phase 2 has 1 active todo, 0 done
  });
});

describe("railRollup — per-status bar zones (done/active/blocked/skipped, todo empty)", () => {
  it("the current-phase bar exposes done + active zones from the phase counts (count/total)", () => {
    // Current phase = phase 1: 2 done, 1 in_progress, 1 todo ⇒ total 4 ⇒ done 50, active 25.
    const d = detail(THREE_PHASES, [
      task(1, "done"),
      task(1, "done"),
      task(1, "in_progress"),
      task(1, "todo"),
    ]);
    const b = bar(railRollup(d));
    expect(zoneH(b, "done")).toBe(50);
    expect(zoneH(b, "active")).toBe(25);
    // Remainder (100 − 50 − 25 = 25%) is the empty TODO track — the zones never overflow.
    expect(zoneH(b, "done") + zoneH(b, "active")).toBeLessThanOrEqual(100);
  });

  it("counts BOTH in_progress AND in_review as the EN-COURS (active) zone", () => {
    // 1 in_progress + 1 in_review + 2 todo ⇒ done 0, active 2 / total 4 ⇒ done 0, active 50.
    const d = detail(ONE_PHASE, [
      task(1, "in_progress"),
      task(1, "in_review"),
      task(1, "todo"),
      task(1, "todo"),
    ]);
    const b = bar(railRollup(d));
    expect(zoneH(b, "done")).toBe(0);
    expect(zoneH(b, "active")).toBe(50);
  });

  it("a BLOCKED task yields a blocked zone in DESTRUCTIVE (red) — the global task-status dot colour", () => {
    // Phase 1: 1 done, 1 blocked, 2 todo ⇒ total 4 ⇒ done 25, blocked 25 (no longer invisible).
    const d = detail(ONE_PHASE, [
      task(1, "done"),
      task(1, "blocked"),
      task(1, "todo"),
      task(1, "todo"),
    ]);
    const b = bar(railRollup(d));
    expect(zoneH(b, "blocked")).toBe(25);
    // Same colour as the panel's blocked task dot (taskStatusMeta) — destructive/red, not grey.
    expect(zoneTone(b, "blocked")).toBe(taskStatusMeta("blocked").dot);
    expect(zoneTone(b, "blocked")).toBe("bg-destructive");
  });

  it("a SKIPPED (cancelled) task yields the neutral skipped zone (global cancelled dot tone)", () => {
    // Phase 1: 1 done, 1 cancelled(skip), 2 todo ⇒ total 4 ⇒ done 25, skipped 25.
    const d = detail(ONE_PHASE, [
      task(1, "done"),
      task(1, "cancelled"),
      task(1, "todo"),
      task(1, "todo"),
    ]);
    const b = bar(railRollup(d));
    expect(zoneH(b, "skipped")).toBe(25);
    expect(zoneTone(b, "skipped")).toBe(taskStatusMeta("cancelled").dot);
  });

  it("every zone's tone = the GLOBAL task-status dot mapping (done=success, active=warning)", () => {
    const d = detail(ONE_PHASE, [task(1, "done"), task(1, "in_progress")]);
    const b = bar(railRollup(d));
    expect(zoneTone(b, "done")).toBe(taskStatusMeta("done").dot);
    expect(zoneTone(b, "done")).toBe("bg-success");
    expect(zoneTone(b, "active")).toBe(taskStatusMeta("in_progress").dot);
    expect(zoneTone(b, "active")).toBe("bg-warning");
  });

  it("stacks the zones top→down in progression order: done → active → blocked → skipped", () => {
    // One task per status ⇒ every zone present ⇒ the full ordered stack.
    const d = detail(ONE_PHASE, [
      task(1, "done"),
      task(1, "in_progress"),
      task(1, "blocked"),
      task(1, "cancelled"),
      task(1, "todo"),
    ]);
    const b = bar(railRollup(d));
    expect(zoneKeys(b)).toEqual(["done", "active", "blocked", "skipped"]);
    // 5 tasks, one each of done/active/blocked/skipped ⇒ 20% per zone; the 5th (todo) is empty track.
    expect(zoneH(b, "done")).toBe(20);
    expect(zoneH(b, "active")).toBe(20);
    expect(zoneH(b, "blocked")).toBe(20);
    expect(zoneH(b, "skipped")).toBe(20);
    // Disjoint statuses ⇒ the zones' heights sum to ≤ 100 (todo remainder is the empty track).
    const sum = b?.segments?.reduce((a, s) => a + s.heightPct, 0) ?? 0;
    expect(sum).toBeLessThanOrEqual(100);
  });

  it("only the ACTIVE zone pulses; done/blocked/skipped do not", () => {
    const d = detail(ONE_PHASE, [
      task(1, "done"),
      task(1, "in_progress"),
      task(1, "blocked"),
      task(1, "cancelled"),
    ]);
    const b = bar(railRollup(d));
    expect(b?.segments?.find((s) => s.key === "active")?.pulse).toBe(true);
    for (const key of ["done", "blocked", "skipped"] as const) {
      expect(b?.segments?.find((s) => s.key === key)?.pulse).toBe(false);
    }
  });

  it("an ABSENT status yields NO segment (todo-only ⇒ no zones at all, empty bar)", () => {
    const d = detail(ONE_PHASE, [task(1, "todo"), task(1, "todo")]);
    const b = bar(railRollup(d));
    expect(b?.segments).toEqual([]);
  });

  it("NO in_progress/in_review ⇒ no active zone (just the solid done zone, no pulse)", () => {
    // Phase 1: 1 done, 1 todo ⇒ done 50, no active segment.
    const d = detail(ONE_PHASE, [task(1, "done"), task(1, "todo")]);
    const b = bar(railRollup(d));
    expect(zoneH(b, "done")).toBe(50);
    expect(zoneH(b, "active")).toBe(0);
    expect(b?.segments?.some((s) => s.key === "active")).toBe(false);
  });

  it("NO tasks ⇒ no zones (empty bar, never NaN)", () => {
    const b = bar(railRollup(detail(THREE_PHASES, [])));
    expect(b?.segments).toEqual([]);
    expect(zoneH(b, "done")).toBe(0);
    expect(zoneH(b, "active")).toBe(0);
    expect(Number.isNaN(zoneH(b, "done"))).toBe(false);
  });

  it("FLAT spec ⇒ the single bar carries zones for the WHOLE task set", () => {
    // 1 done, 1 in_progress, 2 todo ⇒ total 4 ⇒ done 25, active 25.
    const d = detail("[]", [
      task(null, "done"),
      task(null, "in_progress"),
      task(null, "todo"),
      task(null, "todo"),
    ]);
    const b = bar(railRollup(d));
    expect(zoneH(b, "done")).toBe(25);
    expect(zoneH(b, "active")).toBe(25);
  });

  it("barZonesOf: empty set ⇒ []; disjoint zones never exceed total", () => {
    expect(barZonesOf([])).toEqual([]);
    // 1 done, 2 in_progress, 1 in_review ⇒ done 1, active 3 / total 4 ⇒ 25 + 75 = 100.
    const z = barZonesOf([
      task(1, "done"),
      task(1, "in_progress"),
      task(1, "in_progress"),
      task(1, "in_review"),
    ]);
    expect(z.find((s) => s.key === "done")?.heightPct).toBe(25);
    expect(z.find((s) => s.key === "active")?.heightPct).toBe(75);
    expect(z.reduce((a, s) => a + s.heightPct, 0)).toBeLessThanOrEqual(100);
  });

  it("all-done phases (done dots) carry NO segments", () => {
    const rollup = railRollup(detail(THREE_PHASES, [task(1, "done"), task(2, "done"), task(3, "done")]));
    expect(rollup.items.every((it) => it.segments === undefined)).toBe(true);
  });
});

describe("railItemMorph — the dot↔bar morph geometry (purely vertical)", () => {
  it("BAR target is the tall h-16 gauge; DOT target is the tiny round pastille — heights differ", () => {
    expect(railItemMorph("bar").height).toBe(RAIL_BAR_HEIGHT_PX);
    expect(RAIL_BAR_HEIGHT_PX).toBe(64);
    expect(railItemMorph("done").height).toBe(RAIL_DOT_SIZE_PX);
    expect(railItemMorph("remaining").height).toBe(RAIL_DOT_SIZE_PX);
    expect(RAIL_DOT_SIZE_PX).toBe(6);
    // The morph animates a REAL height change between the two states.
    expect(railItemMorph("bar").height).not.toBe(railItemMorph("done").height);
  });

  it("RADIUS differs by kind: the DOT is fully ROUND (size ÷ 2), the BAR is a rounded-RECT", () => {
    // DOT radius = half its size ⇒ a circle; BAR radius is a small rounded-rect, not a pill.
    expect(railItemMorph("done").borderRadius).toBe(RAIL_DOT_SIZE_PX / 2);
    expect(railItemMorph("remaining").borderRadius).toBe(RAIL_DOT_SIZE_PX / 2);
    expect(railItemMorph("bar").borderRadius).not.toBe(railItemMorph("done").borderRadius);
    // The DOT's radius (3) rounds a 6px box to a circle; the BAR keeps square-ish corners.
    expect(railItemMorph("bar").borderRadius).toBeLessThan(RAIL_BAR_HEIGHT_PX / 2);
  });

  it("the WIDTH is CONSTANT across dot and bar ⇒ the morph is purely VERTICAL", () => {
    // A single shared width token (w-1.5) is used for both states — no horizontal morph.
    expect(RAIL_ITEM_WIDTH_PX).toBe(6);
    expect(RAIL_ITEM_WIDTH_PX).toBe(RAIL_DOT_SIZE_PX);
  });
});

describe("currentPhaseIndex", () => {
  it("returns the first not-fully-done phase, or -1 when all are done", () => {
    expect(currentPhaseIndex([[task(1, "done")], [task(2, "todo")]])).toBe(1);
    expect(currentPhaseIndex([[task(1, "done")], [task(2, "done")]])).toBe(-1);
    // An EMPTY phase is not done (nothing completed) ⇒ it is the current phase.
    expect(currentPhaseIndex([[], [task(2, "todo")]])).toBe(0);
  });
});

describe("tasksOfPhase", () => {
  it("filters tasks by their phase index", () => {
    const tasks = [task(1, "done"), task(2, "todo"), task(1, "todo")];
    expect(tasksOfPhase(tasks, 1)).toHaveLength(2);
    expect(tasksOfPhase(tasks, 2)).toHaveLength(1);
    expect(tasksOfPhase(tasks, 3)).toHaveLength(0);
  });
});

describe("rail colour tokens — contrast vs the languette AND the scrollbar", () => {
  it("done point = success (green); remaining point is a VISIBLE neutral, NOT the old blending /35", () => {
    expect(POINT_TONE.done).toBe("bg-success");
    // The old invisible pip was `bg-muted-foreground/35` (~the 45%-opacity scrollbar thumb).
    expect(POINT_TONE.remaining).not.toMatch(/bg-muted-foreground\/35\b/);
    // A more opaque neutral (/60) + a crisp ring so it reads against the languette AND scrollbar.
    expect(POINT_TONE.remaining).toMatch(/bg-muted-foreground\/60/);
    expect(POINT_TONE.remaining).toMatch(/ring/);
  });

  it("the bar zone tones come from the GLOBAL task-status dot mapping and the TRACK contrasts", () => {
    // The active/en-cours zone is the in_progress task-status dot colour (bg-warning), not bespoke.
    const b = bar(railRollup(detail(ONE_PHASE, [task(1, "done"), task(1, "in_progress")])));
    expect(zoneTone(b, "active")).toBe(taskStatusMeta("in_progress").dot);
    expect(zoneTone(b, "active")).toBe("bg-warning");
    // The track is a distinct neutral (not the fill), so the empty portion stays visible.
    expect(BAR_TRACK).not.toBe(zoneTone(b, "active"));
    expect(BAR_TRACK).toMatch(/muted-foreground/);
  });

  it("the DONE zone tone is solid success (green) — the SAME token as the done phase dot", () => {
    const b = bar(railRollup(detail(ONE_PHASE, [task(1, "done"), task(1, "todo")])));
    expect(zoneTone(b, "done")).toBe(taskStatusMeta("done").dot);
    expect(zoneTone(b, "done")).toBe("bg-success");
    expect(zoneTone(b, "done")).toBe(POINT_TONE.done);
  });
});
