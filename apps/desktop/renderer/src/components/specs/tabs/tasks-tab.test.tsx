import { render, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { CommentAuthorType } from "@/bridge";

import { taskStatusMeta } from "../spec-meta";
import type { SpecDetail, Task } from "../types";
import {
  findingProvenance,
  reworkCount,
  severityMeta,
  splitPhaseTasks,
  TasksTab,
} from "./tasks-tab";

/** A `tasks` row fixture (the `nyx_core::db::Task` wire shape). */
function task(overrides: Partial<Task>): Task {
  return {
    id: "t1",
    project_id: "p1",
    spec_id: "s1",
    parent_task_id: null,
    kind: "spec",
    executor: "agent",
    type_: "",
    status: "todo",
    priority: "med",
    severity: null,
    title: "A task",
    description: null,
    acceptance_criteria: null,
    verification_command: null,
    phase: null,
    position: 0,
    blocked_reason: null,
    skip_reason: null,
    created_at: 0,
    updated_at: 0,
    completed_at: null,
    from_comment_id: null,
    grill_state: null,
    round: null,
    ...overrides,
  };
}

/** A `spec_detail` fixture over `phasesJson` (a `[{number,name}]` string) + a task list. */
function detail(phasesJson: string, tasks: Task[]): SpecDetail {
  return {
    spec: {
      id: "s1",
      project_id: "p1",
      workspace_id: "w1",
      title: "Spec",
      slug: null,
      number: 1,
      priority: "med",
      status: "in_review",
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
      id: "r1",
      spec_id: "s1",
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
  } as unknown as SpecDetail;
}

describe("splitPhaseTasks — the round grouping logic (PDR-0022, review F4)", () => {
  it("keeps round null/0 tasks in the PLAN and never groups them", () => {
    const plan0 = task({ id: "a", round: 0 });
    const planNull = task({ id: "b", round: null });
    const { plan, rounds } = splitPhaseTasks([plan0, planNull]);

    expect(plan.map((t) => t.id)).toEqual(["a", "b"]);
    expect(rounds).toEqual([]);
  });

  it("groups round ≥ 1 findings by round, ASCENDING, plan first", () => {
    const planTask = task({ id: "p", round: null });
    const r1a = task({ id: "r1a", kind: "review", round: 1 });
    const r2 = task({ id: "r2", kind: "review", round: 2 });
    const r1b = task({ id: "r1b", kind: "review", round: 1 });

    // Deliberately out of order (r2 before the second r1) to prove the grouping is stable + sorted.
    const { plan, rounds } = splitPhaseTasks([planTask, r1a, r2, r1b]);

    expect(plan.map((t) => t.id)).toEqual(["p"]);
    expect(rounds.map((g) => g.round)).toEqual([1, 2]); // ascending
    expect(rounds[0].tasks.map((t) => t.id)).toEqual(["r1a", "r1b"]); // same-round order preserved
    expect(rounds[1].tasks.map((t) => t.id)).toEqual(["r2"]);
  });

  it("is empty-safe (no tasks ⇒ empty plan + no rounds)", () => {
    expect(splitPhaseTasks([])).toEqual({ plan: [], rounds: [] });
  });
});

describe("<TasksTab> — findings render as round sub-phases N.1 / N.2 (review F4)", () => {
  // Phase 3 ("Column model") with a plan task (round 0) + two review passes: round 1 (two findings)
  // and round 2 (one finding). Phases 1 & 2 exist so phase 3's DISPLAY number is 3 ⇒ labels 3.1/3.2.
  const phases =
    '[{"number":1,"name":"One"},{"number":2,"name":"Two"},{"number":3,"name":"Column model"}]';
  const tasks: Task[] = [
    task({ id: "plan-3", title: "Plan task", phase: 3, round: null, status: "done" }),
    task({ id: "f-3-1a", title: "Finding A", kind: "review", phase: 3, round: 1, status: "done" }),
    task({ id: "f-3-1b", title: "Finding B", kind: "review", phase: 3, round: 1, status: "todo" }),
    task({ id: "f-3-2a", title: "Finding C", kind: "review", phase: 3, round: 2, status: "todo" }),
  ];

  it("renders a sub-header 3.1 and 3.2, distinct from the plan tasks", () => {
    const { container } = render(<TasksTab detail={detail(phases, tasks)} />);

    const groups = [...container.querySelectorAll<HTMLElement>("[data-round-group]")];
    // Two round sub-groups (round 1 and round 2), in order.
    expect(groups.map((g) => g.dataset.roundGroup)).toEqual(["1", "2"]);
    // Their pastilles read "3.1" / "3.2" (phase display number . round).
    const labels = groups.map(
      (g) => g.querySelector<HTMLElement>("[data-group-label]")?.textContent,
    );
    expect(labels).toEqual(["3.1", "3.2"]);
  });

  it("puts each finding inside its own round sub-group (not flat among plan tasks)", () => {
    const { container } = render(<TasksTab detail={detail(phases, tasks)} />);

    const groupOf = (round: string) =>
      container.querySelector<HTMLElement>(`[data-round-group="${round}"]`)!;

    // Round 3.1 holds Finding A + Finding B; round 3.2 holds Finding C.
    expect(within(groupOf("1")).getByText("Finding A")).toBeTruthy();
    expect(within(groupOf("1")).getByText("Finding B")).toBeTruthy();
    expect(within(groupOf("2")).getByText("Finding C")).toBeTruthy();

    // The plan task is NOT inside any round sub-group.
    expect(within(groupOf("1")).queryByText("Plan task")).toBeNull();
    expect(within(groupOf("2")).queryByText("Plan task")).toBeNull();
  });

  it("PHASE bar counts the PLAN only; each round carries its OWN fraction (finding #36)", () => {
    const { container } = render(<TasksTab detail={detail(phases, tasks)} />);

    // Phase 3's header tally = PLAN only: one plan task, done ⇒ 1/1 (findings EXCLUDED — no 2/4).
    const phaseTally = container.querySelector<HTMLElement>(
      '[data-phase-group="3"] [data-group-tally]',
    );
    expect(phaseTally?.textContent).toBe("1/1");

    // Each round owns its fraction: 3.1 = [done, todo] ⇒ 1/2, 3.2 = [todo] ⇒ 0/1.
    const roundTally = (r: string) =>
      container.querySelector<HTMLElement>(`[data-round-group="${r}"] [data-group-tally]`)
        ?.textContent;
    expect(roundTally("1")).toBe("1/2");
    expect(roundTally("2")).toBe("0/1");
  });

  it("a Spec with NO review rounds renders no round sub-groups (non-regression)", () => {
    const plain = detail('[{"number":1,"name":"One"}]', [
      task({ id: "a", title: "Plain task", phase: 1, round: null }),
    ]);
    const { container } = render(<TasksTab detail={plain} />);

    expect(container.querySelectorAll("[data-round-group]")).toHaveLength(0);
    // The plan tasks render DIRECTLY under the phase group header — no round-0 / "Plan initial" wrapper.
    expect(container.querySelector("[data-phase-group]")).not.toBeNull();
    expect(container.textContent).not.toMatch(/Plan initial/);
  });
});

describe("taskStatusMeta — the complete status→colour model (dot tone + struck), source of truth", () => {
  it("maps the statuses to their design-system dot tones (done/in_progress/blocked/todo/cancelled)", () => {
    expect(taskStatusMeta("done").dot).toBe("bg-success"); // green
    expect(taskStatusMeta("in_progress").dot).toBe("bg-warning"); // amber
    expect(taskStatusMeta("blocked").dot).toBe("bg-destructive"); // red
    expect(taskStatusMeta("todo").dot).toBe("bg-muted-foreground/50"); // full grey
    expect(taskStatusMeta("cancelled").dot).toBe("bg-muted-foreground/28"); // attenuated grey
  });

  it("cancelled is DISTINCT from todo — attenuated grey + struck vs full grey + not struck", () => {
    const todo = taskStatusMeta("todo");
    const cancelled = taskStatusMeta("cancelled");
    expect(cancelled.dot).not.toBe(todo.dot); // a weaker grey than todo
    expect(todo.struck).toBe(false); // todo title NOT struck
    expect(cancelled.struck).toBe(true); // cancelled title struck
  });

  it("struck flags exactly done + cancelled (a skip stamps cancelled); the rest are not struck", () => {
    expect(taskStatusMeta("done").struck).toBe(true);
    expect(taskStatusMeta("cancelled").struck).toBe(true);
    expect(taskStatusMeta("todo").struck).toBe(false);
    expect(taskStatusMeta("in_progress").struck).toBe(false);
    expect(taskStatusMeta("blocked").struck).toBe(false);
  });

  it("defaults an unknown status to todo (full grey, not struck)", () => {
    expect(taskStatusMeta("mystery")).toEqual(taskStatusMeta("todo"));
  });
});

describe("<TasksTab> — per-row status look: dot tone + title strike (cancelled ≠ todo ≠ done)", () => {
  const phases = '[{"number":1,"name":"One"}]';
  const tasks: Task[] = [
    task({ id: "d", title: "Done row", phase: 1, status: "done" }),
    task({ id: "p", title: "Progress row", phase: 1, status: "in_progress" }),
    task({ id: "b", title: "Blocked row", phase: 1, status: "blocked" }),
    task({ id: "t", title: "Todo row", phase: 1, status: "todo" }),
    task({ id: "c", title: "Cancelled row", phase: 1, status: "cancelled" }),
  ];

  const rowOf = (c: HTMLElement, title: string) =>
    within(c).getByText(title).closest("div.group") as HTMLElement;
  const dotOf = (c: HTMLElement, title: string) =>
    rowOf(c, title).querySelector("span[aria-hidden].rounded-full") as HTMLElement;
  const titleOf = (c: HTMLElement, title: string) => within(c).getByText(title);

  it("paints each status dot with its distinct token (green/amber/red/grey/attenuated)", () => {
    const { container } = render(<TasksTab detail={detail(phases, tasks)} />);
    expect(dotOf(container, "Done row").className).toContain("bg-success");
    expect(dotOf(container, "Progress row").className).toContain("bg-warning");
    expect(dotOf(container, "Blocked row").className).toContain("bg-destructive");
    expect(dotOf(container, "Todo row").className).toContain("bg-muted-foreground/50");
    expect(dotOf(container, "Cancelled row").className).toContain("bg-muted-foreground/28");
    // cancelled dot is a DIFFERENT grey from todo (indistinguishable-grey bug fixed).
    expect(dotOf(container, "Cancelled row").className).not.toBe(
      dotOf(container, "Todo row").className,
    );
  });

  it("strikes done + cancelled titles ONLY (todo/in_progress/blocked stay upright)", () => {
    const { container } = render(<TasksTab detail={detail(phases, tasks)} />);
    expect(titleOf(container, "Done row").className).toContain("line-through");
    expect(titleOf(container, "Cancelled row").className).toContain("line-through");
    expect(titleOf(container, "Todo row").className).not.toContain("line-through");
    expect(titleOf(container, "Progress row").className).not.toContain("line-through");
    expect(titleOf(container, "Blocked row").className).not.toContain("line-through");
  });
});

describe("provenance — derived from the source comment's author_type (NO origin column)", () => {
  const authors = new Map<string, CommentAuthorType>([
    ["c-human", "human"],
    ["c-agent", "agent"],
  ]);

  it("findingProvenance reads the author of from_comment_id (human vs agent)", () => {
    expect(findingProvenance(task({ from_comment_id: "c-human" }), authors)).toBe("human");
    expect(findingProvenance(task({ from_comment_id: "c-agent" }), authors)).toBe("agent");
  });

  it("defaults to agent (the auditor) when the source is unknown or absent", () => {
    expect(findingProvenance(task({ from_comment_id: "c-missing" }), authors)).toBe("agent");
    expect(findingProvenance(task({ from_comment_id: null }), authors)).toBe("agent");
    expect(findingProvenance(task({ from_comment_id: "c-human" }))).toBe("agent"); // no map ⇒ agent
  });
});

describe("reworkCount / severityMeta", () => {
  it("reworkCount = distinct (phase, round ≥ 1) groups across the Spec", () => {
    const tasks = [
      task({ phase: 1, round: null }), // plan — not a rework
      task({ phase: 1, round: 1 }),
      task({ phase: 1, round: 1 }), // same group
      task({ phase: 1, round: 2 }),
      task({ phase: 2, round: 1 }), // a different phase's round 1
    ];
    expect(reworkCount(tasks)).toBe(3); // 1.1, 1.2, 2.1
  });

  it("severityMeta maps high⇒destructive, med⇒warning, low⇒muted (null ⇒ no badge)", () => {
    expect(severityMeta("high")?.variant).toBe("destructive");
    expect(severityMeta("critical")?.variant).toBe("destructive");
    expect(severityMeta("medium")?.variant).toBe("warning");
    expect(severityMeta("low")?.variant).toBe("muted");
    expect(severityMeta(null)).toBeNull();
  });
});

describe("<TasksTab> — panel render (phase/round groups identical, plan-only, provenance, English)", () => {
  const phases = '[{"number":1,"name":"One"},{"number":2,"name":"Column model"}]';
  const authors = new Map<string, CommentAuthorType>([
    ["c-human", "human"],
    ["c-agent", "agent"],
  ]);
  const tasks: Task[] = [
    task({ id: "plan-2", title: "Plan task", phase: 2, round: null, status: "done" }),
    task({
      id: "f-2-1a",
      title: "Human finding",
      kind: "review",
      phase: 2,
      round: 1,
      status: "done",
      severity: "high",
      from_comment_id: "c-human",
    }),
    task({
      id: "f-2-2a",
      title: "Auditor finding",
      kind: "review",
      phase: 2,
      round: 2,
      status: "todo",
      severity: "low",
      from_comment_id: "c-agent",
    }),
  ];

  const renderPanel = () =>
    render(<TasksTab detail={detail(phases, tasks)} authorByCommentId={authors} />);

  it("roll-up header carries the rework counter", () => {
    const { container } = renderPanel();
    // Two rework groups (2.1 + 2.2) ⇒ the header rework counter reads 2.
    const rollup = container.querySelector("[data-rework-count]");
    expect(rollup?.getAttribute("data-rework-count")).toBe("2");
  });

  it("renders NO round-0 / 'Plan initial' sub-header — the plan sits under the phase header", () => {
    const { container } = renderPanel();
    // The plan task renders directly; there is no separate plan wrapper and no "Plan initial" label.
    expect(container.textContent).not.toMatch(/Plan initial/);
    // Round pastilles read the phase display number . round ("2.1" / "2.2").
    const labels = [...container.querySelectorAll<HTMLElement>("[data-round-group]")].map(
      (g) => g.querySelector<HTMLElement>("[data-group-label]")?.textContent,
    );
    expect(labels).toEqual(["2.1", "2.2"]);
  });

  it("round header is STRICTLY identical to the phase header (same background/border/layout)", () => {
    const { container } = renderPanel();
    const phaseHeader = container.querySelector<HTMLElement>(
      "[data-phase-group] > [data-group-header]",
    );
    const roundHeaders = [
      ...container.querySelectorAll<HTMLElement>("[data-round-group] > [data-group-header]"),
    ];
    expect(phaseHeader).not.toBeNull();
    expect(roundHeaders).toHaveLength(2);
    // Byte-identical header chrome ⇒ same fond/bordure/layout — no asymmetric gap (the user's return).
    for (const rh of roundHeaders) expect(rh.className).toBe(phaseHeader!.className);
    expect(phaseHeader!.className).toContain("bg-muted/40");
  });

  it("PHASE bar is plan-only; each round owns its bar/fraction", () => {
    const { container } = renderPanel();
    // Phase 2 plan = one done task ⇒ 1/1 (findings excluded). Round 2.1 = 1/1 done, round 2.2 = 0/1.
    expect(
      container.querySelector<HTMLElement>('[data-phase-group="2"] [data-group-tally]')
        ?.textContent,
    ).toBe("1/1");
    const roundTally = (r: string) =>
      container.querySelector<HTMLElement>(`[data-round-group="${r}"] [data-group-tally]`)
        ?.textContent;
    expect(roundTally("1")).toBe("1/1");
    expect(roundTally("2")).toBe("0/1");
  });

  it("provenance shows ONLY as the per-finding icon — never in the round header", () => {
    const { container } = renderPanel();
    // No provenance data-attr / text on any round header anymore.
    expect(container.querySelector("[data-round-provenance]")).toBeNull();
    // The finding ROW still carries the tinted provenance glyph: human ⇒ text-primary, auditor ⇒ text-info.
    const round1 = container.querySelector<HTMLElement>('[data-round-group="1"]')!;
    const round2 = container.querySelector<HTMLElement>('[data-round-group="2"]')!;
    expect(round1.querySelector("svg.text-primary")).toBeTruthy(); // human finding
    expect(round2.querySelector("svg.text-info")).toBeTruthy(); // auditor finding
    // Per-finding severity badge still renders (high on round 2.1).
    expect(within(round1).getByText("high")).toBeTruthy();
  });

  it("renders no FR strings — the panel is fully English", () => {
    const { container } = renderPanel();
    expect(container.textContent ?? "").not.toMatch(/retour humain|auditeur|Plan initial/i);
  });
});
