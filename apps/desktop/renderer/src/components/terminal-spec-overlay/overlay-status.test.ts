import { describe, expect, it } from "vitest";

import { taskStatusMeta } from "../specs/spec-meta";
import type { SpecDetail, Task } from "../specs/types";
import {
  currentPhaseIndex,
  phaseUnits,
  RAIL_DOT_TONE,
  RAIL_NEEDS_TONE,
  RAIL_TRACK_TONE,
  railModel,
  roundCount,
  segmentOf,
  tasksOfPhase,
  type RailPhase,
  type RailUnit,
} from "./overlay-status";

/** A minimal `tasks` row — the rail reads `phase`/`status`/`kind`/`round`. */
function task(phase: number | null, status: string, extra: Partial<Task> = {}): Task {
  return {
    id: `t-${phase}-${status}-${extra.round ?? 0}-${Math.random().toString(36).slice(2, 6)}`,
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
    from_comment_id: null,
    grill_state: null,
    round: null,
    ...extra,
  };
}

/** A review FINDING (kind=review) rattached to `phase` + `round`. */
function finding(phase: number, status: string, round: number): Task {
  return task(phase, status, { kind: "review", severity: "high", round });
}

/** A minimal `spec_detail` — the rail reads only `spec.phases` (JSON) + `tasks`. */
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
const ONE_PHASE = JSON.stringify([{ number: 1, name: "P1" }]);

/** The ordered `kind` sequence of the rail — one per phase, top→bottom. */
function kinds(model: { phases: RailPhase[] }): string[] {
  return model.phases.map((p) => p.kind);
}
/** The single CURRENT phase (the coloured bar), or `undefined`. */
function currentPhase(model: {
  phases: RailPhase[];
}): Extract<RailPhase, { kind: "current" }> | undefined {
  return model.phases.find(
    (p): p is Extract<RailPhase, { kind: "current" }> => p.kind === "current",
  );
}
/** The ordered segment STATES of a unit (one per non-cancelled task, top→bottom). */
function segStates(u: RailUnit | undefined): string[] {
  return (u?.segments ?? []).map((s) => s.state);
}

describe("railModel — one entry per phase, the current phase is the ONLY coloured bar", () => {
  it("PAST = grey dot before the current phase, CURRENT = the bar, FUTURE = grey dot after", () => {
    // Phase 1 done, phase 2 half-done (current), phase 3 untouched.
    const d = detail(THREE_PHASES, [
      task(1, "done"),
      task(2, "done"),
      task(2, "todo"),
      task(3, "todo"),
    ]);
    const model = railModel(d);
    expect(kinds(model)).toEqual(["past", "current", "future"]);
    expect(model.phases).toHaveLength(3); // one entry per phase, never overflows
    // The current phase (phase 2) is a single plan unit = a strip of one segment per task.
    const cur = currentPhase(model);
    expect(cur?.units).toHaveLength(1);
    expect(cur?.units[0].key).toBe("plan");
    expect(segStates(cur?.units[0])).toEqual(["done", "todo"]);
  });

  it("EVERY phase done (Spec finished) ⇒ the LAST phase is an all-GREEN current bar, rest are grey dots", () => {
    const d = detail(THREE_PHASES, [task(1, "done"), task(2, "done"), task(3, "done")]);
    const model = railModel(d);
    // dots stay grey (past) except the last phase, which reads as the green "done" bar (tout vert).
    expect(kinds(model)).toEqual(["past", "past", "current"]);
    expect(segStates(currentPhase(model)?.units[0])).toEqual(["done"]);
  });

  it("FLAT spec (no phases) ⇒ a SINGLE current bar over the whole task set", () => {
    const d = detail("[]", [task(null, "done"), task(null, "todo"), task(null, "todo")]);
    const model = railModel(d);
    expect(kinds(model)).toEqual(["current"]);
    // One segment per task, in task order.
    expect(segStates(currentPhase(model)?.units[0])).toEqual(["done", "todo", "todo"]);
  });

  it("NO tasks ⇒ phase 1 is the current bar (empty, no segments), later phases are future dots", () => {
    const model = railModel(detail(THREE_PHASES, []));
    expect(kinds(model)).toEqual(["current", "future", "future"]);
    // An empty unit has no segments — it renders as a bare track.
    expect(currentPhase(model)?.units[0].segments).toEqual([]);
  });

  it("a PAST/FUTURE phase carries its rework-ROUND count as hollow rings; a plain phase carries 0", () => {
    // Phase 1 fully done WITH one rework round (also done) ⇒ a past dot with 1 ring. Phase 2 current.
    const d = detail(THREE_PHASES, [
      task(1, "done"),
      finding(1, "done", 1),
      task(2, "todo"),
      task(3, "todo"),
    ]);
    const model = railModel(d);
    expect(model.phases[0]).toEqual({ kind: "past", rounds: 1 });
    expect(model.phases[2]).toEqual({ kind: "future", rounds: 0 });
  });

  it("the current phase's UNITS = plan then one per rework round, ascending", () => {
    // Phase 1 current: a plan slice (done) + round 1 (done) + round 2 (in progress).
    const d = detail(ONE_PHASE, [
      task(1, "done"),
      finding(1, "done", 1),
      finding(1, "in_progress", 2),
    ]);
    const cur = currentPhase(railModel(d));
    expect(cur?.units.map((u) => u.key)).toEqual(["plan", "round-1", "round-2"]);
    // Each unit is a one-segment strip coloured by its task's status.
    expect(segStates(cur?.units[0])).toEqual(["done"]);
    expect(segStates(cur?.units[1])).toEqual(["done"]);
    expect(segStates(cur?.units[2])).toEqual(["in_progress"]);
    // The plan unit is NOT hatched; the round units ARE (rework bg).
    expect(cur?.units[0].rework).toBe(false);
    expect(cur?.units[1].rework).toBe(true);
    expect(cur?.units[2].rework).toBe(true);
  });

  it("a blocked task surfaces as a RED segment; an in_progress task at 0 done still shows AMBER (the bug)", () => {
    // blocked among done ⇒ its own red segment (no aggregate phase hue).
    const blocked = currentPhase(
      railModel(detail(ONE_PHASE, [task(1, "done"), task(1, "blocked")])),
    );
    expect(segStates(blocked?.units[0])).toEqual(["done", "blocked"]);
    // 1 in_progress + 2 todo, 0 done: the amber segment is visible (not a fully-grey bar).
    const active = currentPhase(
      railModel(detail(ONE_PHASE, [task(1, "in_progress"), task(1, "todo"), task(1, "todo")])),
    );
    const segs = active?.units[0].segments ?? [];
    expect(segs.map((s) => s.state)).toEqual(["in_progress", "todo", "todo"]);
    expect(segs[0].transparent).toBe(false); // amber painted
    expect(segs[0].tone).toBe(taskStatusMeta("in_progress").dot);
    expect(segs[1].transparent).toBe(true); // todo = opacity 0
    expect(segs[2].transparent).toBe(true);
  });
});

describe("segmentOf — one segment per task, coloured by its STATUS via the SAME taskStatusMeta as the dots", () => {
  it("done/in_progress/blocked use the panel dot tone (dot ↔ segment coherence); todo is transparent", () => {
    const done = segmentOf(task(1, "done"));
    expect(done).toMatchObject({
      state: "done",
      tone: taskStatusMeta("done").dot,
      transparent: false,
      pulse: false,
    });

    const prog = segmentOf(task(1, "in_progress"));
    expect(prog).toMatchObject({
      state: "in_progress",
      tone: taskStatusMeta("in_progress").dot,
      transparent: false,
      pulse: true, // in_progress breathes
    });

    const blocked = segmentOf(task(1, "blocked"));
    expect(blocked).toMatchObject({
      state: "blocked",
      tone: taskStatusMeta("blocked").dot,
      transparent: false,
      pulse: true, // blocked breathes
    });

    const todo = segmentOf(task(1, "todo"));
    expect(todo).toMatchObject({ state: "todo", transparent: true, pulse: false });
  });

  it("a pending kind=human task ⇒ a magenta needs_you segment that pulses", () => {
    const seg = segmentOf(task(1, "todo", { kind: "human" }));
    expect(seg).toMatchObject({
      state: "needs_you",
      tone: RAIL_NEEDS_TONE,
      transparent: false,
      pulse: true,
    });
    expect(RAIL_NEEDS_TONE).toBe("bg-primary"); // magenta in dark
  });

  it("a RESOLVED kind=human task falls back to its status colour (done = green, not magenta)", () => {
    const seg = segmentOf(task(1, "done", { kind: "human" }));
    expect(seg.state).toBe("done");
    expect(seg.tone).toBe(taskStatusMeta("done").dot);
  });
});

describe("phaseUnits — the segment strip of each unit (cancelled excluded)", () => {
  it("always yields at least one unit (an empty phase ⇒ a single empty plan unit, no segments)", () => {
    expect(phaseUnits([])).toEqual([{ key: "plan", rework: false, segments: [] }]);
  });

  it("one segment per task, in task order, split across plan/round units", () => {
    const units = phaseUnits([
      task(1, "done"),
      task(1, "in_progress"),
      finding(1, "todo", 1),
      finding(1, "done", 1),
    ]);
    expect(units[0].key).toBe("plan");
    expect(units[0].rework).toBe(false);
    expect(segStates(units[0])).toEqual(["done", "in_progress"]);
    expect(units[1].key).toBe("round-1");
    expect(units[1].rework).toBe(true);
    expect(segStates(units[1])).toEqual(["todo", "done"]);
  });

  it("1 in_progress + 2 todo ⇒ 1 amber segment + 2 transparent (the 0-done visibility fix)", () => {
    const units = phaseUnits([task(1, "in_progress"), task(1, "todo"), task(1, "todo")]);
    const segs = units[0].segments;
    expect(segs.map((s) => s.state)).toEqual(["in_progress", "todo", "todo"]);
    expect(segs.filter((s) => s.transparent)).toHaveLength(2); // the two todo segments
    expect(segs.filter((s) => !s.transparent)).toHaveLength(1); // the amber in_progress segment
  });

  it("EXCLUDES cancelled tasks (2 done + 1 cancelled ⇒ 2 green segments, NO cancelled segment)", () => {
    const units = phaseUnits([task(1, "done"), task(1, "done"), task(1, "cancelled")]);
    expect(segStates(units[0])).toEqual(["done", "done"]); // exactly 2 — the cancelled one has no segment
    // Mixed: 1 done + 1 todo + 1 cancelled ⇒ 2 segments (done, todo), cancelled dropped.
    const mixed = phaseUnits([task(1, "done"), task(1, "todo"), task(1, "cancelled")]);
    expect(segStates(mixed[0])).toEqual(["done", "todo"]);
  });
});

describe("roundCount / currentPhaseIndex / tasksOfPhase", () => {
  it("roundCount counts DISTINCT rework rounds (round ≥ 1) only", () => {
    expect(roundCount([task(1, "done"), finding(1, "done", 1), finding(1, "todo", 1)])).toBe(1);
    expect(roundCount([finding(1, "done", 1), finding(1, "todo", 2)])).toBe(2);
    expect(roundCount([task(1, "done")])).toBe(0);
  });

  it("currentPhaseIndex = first not-fully-done phase, or -1 when all done", () => {
    expect(currentPhaseIndex([[task(1, "done")], [task(2, "todo")]])).toBe(1);
    expect(currentPhaseIndex([[task(1, "done")], [task(2, "done")]])).toBe(-1);
    expect(currentPhaseIndex([[], [task(2, "todo")]])).toBe(0); // an empty phase is not done
  });

  it("tasksOfPhase filters by the task's phase index", () => {
    const tasks = [task(1, "done"), task(2, "todo"), task(1, "todo")];
    expect(tasksOfPhase(tasks, 1)).toHaveLength(2);
    expect(tasksOfPhase(tasks, 3)).toHaveLength(0);
  });
});

describe("rail colour tokens — dots grey, needs magenta, track neutral", () => {
  it("the phase dot is a neutral GREY, NEVER a status colour (never the green done token)", () => {
    expect(RAIL_DOT_TONE).toMatch(/muted-foreground/);
    expect(RAIL_DOT_TONE).not.toBe(taskStatusMeta("done").dot);
  });

  it("the needs tone is magenta (primary); the unit track is neutral grey", () => {
    expect(RAIL_NEEDS_TONE).toBe("bg-primary");
    expect(RAIL_TRACK_TONE).toMatch(/muted-foreground/);
  });
});
