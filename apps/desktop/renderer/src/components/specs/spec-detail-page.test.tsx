import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { emit, mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";
import { createQueryWrapper } from "@/lib/test-query";

import type { SpecDetail } from "./types";

/** A full `spec_detail` fixture: a Spec anchored on a workspace, with tasks + a dependency. */
function detailFixture(): SpecDetail {
  return {
    spec: {
      id: "s1",
      project_id: "p1",
      workspace_id: "w1",
      title: "The detailed spec",
      slug: "the-detailed-spec",
      number: 7,
      priority: "high",
      status: "in_progress",
      mode: "stepped",
      phases: JSON.stringify([
        { number: 1, name: "Foundation" },
        { number: 2, name: "UI" },
      ]),
      current_revision_number: 2,
      position: 0,
      created_at: 0,
      updated_at: 0,
      completed_at: null,
      archived_at: null,
    },
    revision: {
      id: "r1",
      spec_id: "s1",
      revision_number: 2,
      revision_status: "active",
      title: "The detailed spec",
      problem: "The problem statement.",
      solution: "The solution outline.",
      implementation_decisions: "",
      testing_decisions: "",
      user_stories: "",
      out_of_scope: "",
      change_reason: null,
      created_at: 0,
    },
    tasks: [
      {
        id: "t1",
        project_id: "p1",
        spec_id: "s1",
        parent_task_id: null,
        kind: "slice",
        executor: "agent",
        type_: "implementation",
        status: "in_progress",
        priority: "med",
        severity: null,
        title: "Build the schema",
        description: null,
        acceptance_criteria: null,
        verification_command: null,
        phase: 1,
        position: 0,
        blocked_reason: null,
        skip_reason: null,
        created_at: 0,
        updated_at: 0,
        completed_at: null,
        from_comment_id: null,
        grill_state: null,
        round: null,
      },
      {
        id: "t2",
        project_id: "p1",
        spec_id: "s1",
        parent_task_id: null,
        kind: "slice",
        executor: "agent",
        type_: "implementation",
        status: "blocked",
        priority: "high",
        severity: null,
        title: "Wire the UI",
        description: null,
        acceptance_criteria: null,
        verification_command: null,
        phase: 2,
        position: 0,
        blocked_reason: "waiting on schema",
        skip_reason: null,
        created_at: 0,
        updated_at: 0,
        completed_at: null,
        from_comment_id: null,
        grill_state: null,
        round: null,
      },
    ],
    dependencies: [{ task_id: "t2", depends_on_task_id: "t1", type_: "blocks", created_at: 0 }],
    workspace_context: {
      spec_id: "s1",
      workspace: {
        id: "w1",
        project_id: "p1",
        name: "spec-7-worktree",
        path: "/tmp/wt/spec-7",
        branch: "spec-7",
        is_root: false,
        folder_removed_at: null,
      },
      project: { id: "p1", name: "Nyx", key: "NYX" },
      repos: [
        {
          id: "repo1",
          project_id: "p1",
          name: "core",
          relative_path: ".",
          optional: false,
        },
      ],
    },
  };
}

/** The ndev graph + a run bound to the Spec — what the header's ORCHESTRATION RUN modal reads. */
const ENGINE_WORKFLOW = {
  id: "wf1",
  project_id: null,
  name: "ndev — dev loop (default)",
  scope: "ndev",
  root_node_id: "eroot",
  forked_from: null,
  created_at: 0,
  updated_at: 0,
};
const ENGINE_NODES = [
  {
    id: "eroot",
    workflow_id: "wf1",
    parent_id: null,
    order_index: 0,
    type_: "sequence",
    label: "/ndev",
    config: "{}",
    created_at: 0,
    updated_at: 0,
  },
  {
    id: "ecoder",
    workflow_id: "wf1",
    parent_id: "eroot",
    order_index: 0,
    type_: "spawn_subagent",
    label: "coder",
    config: JSON.stringify({ agent_id: "coder" }),
    created_at: 0,
    updated_at: 0,
  },
];
const ENGINE_RUN = {
  id: "run1",
  workflow_id: "wf1",
  spec_id: "s1",
  parent_run_id: null,
  current_node_id: "ecoder",
  blackboard: JSON.stringify({ phase: "1/3" }),
  status: "running",
  started_at: 0,
  updated_at: 0,
};

function installBackend(detail: SpecDetail = detailFixture(), specRuns: unknown[] = []) {
  mockIPC((cmd) => {
    switch (cmd) {
      case "list_projects":
        return [];
      case "list_workspaces":
        return [];
      case "spec_detail":
        return detail;
      case "engine_spec_runs":
        return specRuns;
      case "engine_workflow_graph":
        return { workflow: ENGINE_WORKFLOW, nodes: ENGINE_NODES };
      case "engine_effector_context":
        return { context: "=== NYX CONTEXT — CODER ===" };
      case "workspace_timeline":
        return { workspace_id: "w1", entries: [] };
      case "spec_activity":
        return { spec_id: "s1", entries: [] };
      case "workspace_runtime_snapshot":
        return {
          workspace: { id: "w1", name: "wt", path: "/tmp" },
          terminals: [],
          agent_sessions: [],
          services: [],
        };
      default:
        return undefined;
    }
  });
}

function renderDetail() {
  const router = createAppRouter(["/work/s1"]);
  // The Review tab's hooks are TanStack Query now (PDR-0029) — render under a fresh query client.
  const { wrapper } = createQueryWrapper();
  return render(<RouterProvider router={router} />, { wrapper });
}

describe("SpecDetailPage", () => {
  beforeEach(() => {
    installBackend();
  });

  it("renders the header (title + human key) and every tab", async () => {
    renderDetail();
    expect(await screen.findByRole("heading", { name: "The detailed spec" })).toBeTruthy();
    // The human key NYX-7 (project key derived from the workspace context).
    expect(screen.getByText("NYX-7")).toBeTruthy();
    // All six tabs are present, in the validated order.
    for (const label of ["Spec", "Tasks", "Review", "Annexes", "Activity", "Runtime"]) {
      expect(screen.getByRole("tab", { name: new RegExp(label) })).toBeTruthy();
    }
  });

  // The RUNTIME lives with the Spec now (`/engine` is structural): the header carries the entry
  // point onto the orchestration-run modal — the shared engine graph, the run's cursor on it, and
  // the blackboard following the run's variables.
  it("opens the orchestration-run modal from the header (graph + blackboard for the Spec's run)", async () => {
    installBackend(detailFixture(), [ENGINE_RUN]);
    const { baseElement } = renderDetail();
    await screen.findByRole("heading", { name: "The detailed spec" });

    // Closed by default — no graph leaked into the page.
    expect(baseElement.querySelector('[data-testid="engine-canvas"]')).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /orchestration run/i }));
    expect(await screen.findByText("Orchestration run")).toBeTruthy();
    await waitFor(() =>
      expect(baseElement.querySelector('[data-node-type="spawn_subagent"]')).toBeTruthy(),
    );
    await waitFor(() => expect(baseElement.querySelector('[data-current="true"]')).toBeTruthy());
    expect(screen.getByText("Blackboard")).toBeTruthy();
    expect(screen.getByText("1/3")).toBeTruthy();
  });

  it("shows the run modal's empty state for a Spec that has never been orchestrated", async () => {
    renderDetail(); // the default backend serves NO run for the Spec
    await screen.findByRole("heading", { name: "The detailed spec" });
    fireEvent.click(screen.getByRole("button", { name: /orchestration run/i }));
    expect(await screen.findByText("No run for this Spec yet")).toBeTruthy();
  });

  it("shows the Spec revision sections on the default tab", async () => {
    renderDetail();
    expect(await screen.findByText("The problem statement.")).toBeTruthy();
    expect(screen.getByText("The solution outline.")).toBeTruthy();
    // The active revision marker.
    expect(screen.getByText(/revision v2/)).toBeTruthy();
  });

  it("consumes the task model on the Tasks tab (phases + blocked-by from dependencies)", async () => {
    renderDetail();
    await screen.findByRole("heading", { name: "The detailed spec" });
    fireEvent.click(screen.getByRole("tab", { name: /Tasks/ }));
    // The two tasks, grouped under their phases.
    expect(await screen.findByText("Build the schema")).toBeTruthy();
    expect(screen.getByText("Wire the UI")).toBeTruthy();
    expect(screen.getByText("Foundation")).toBeTruthy();
    expect(screen.getByText("UI")).toBeTruthy();
    // The blocked task shows what it is blocked by (from the dependency edge). The caption
    // text is split across nodes ("Blocked by: " + the title), so match with `exact: false`.
    expect(screen.getAllByText(/Blocked by:/, { exact: false }).length).toBeGreaterThan(0);
  });

  it("never hides a mal-phased task: a phase absent from the plan lands in Unphased", async () => {
    // A task whose `phase` (99) matches NO plan entry — legacy/mal-phased data. The redesign
    // makes orphans impossible for new Specs, but the UI guard must still surface such a task
    // (never hidden) under the "Unphased" group.
    const base = detailFixture();
    const orphan = { ...base.tasks[0], id: "t99", title: "Orphaned legacy task", phase: 99 };
    const detail: SpecDetail = { ...base, tasks: [...base.tasks, orphan] };
    installBackend(detail);

    renderDetail();
    await screen.findByRole("heading", { name: "The detailed spec" });
    fireEvent.click(screen.getByRole("tab", { name: /Tasks/ }));
    // The orphan is visible, grouped under "Unphased" — not swallowed.
    expect(await screen.findByText("Orphaned legacy task")).toBeTruthy();
    expect(screen.getByText("Unphased")).toBeTruthy();
  });

  // A single-phase Spec fixture built from a status list, e.g. mkPhaseDetail(["done", "cancelled"]).
  // Each entry becomes one task in phase 1; a "cancelled" status is what a `skip` stamps.
  function mkPhaseDetail(statuses: string[]): SpecDetail {
    const base = detailFixture();
    const tasks = statuses.map((status, i) => ({
      ...base.tasks[0],
      id: `t${i}`,
      title: `Task ${i} (${status})`,
      phase: 1,
      status,
      skip_reason: status === "cancelled" ? "obsolete" : null,
      blocked_reason: null,
    }));
    return {
      ...base,
      spec: { ...base.spec, phases: JSON.stringify([{ number: 1, name: "Foundation" }]) },
      tasks,
      dependencies: [],
    };
  }

  async function openTasksTab() {
    renderDetail();
    await screen.findByRole("heading", { name: "The detailed spec" });
    fireEvent.click(screen.getByRole("tab", { name: /Tasks/ }));
  }

  const hasBar = (done: number, total: number) =>
    screen
      .getAllByRole("progressbar")
      .some(
        (b) =>
          b.getAttribute("aria-valuenow") === String(done) &&
          b.getAttribute("aria-valuemax") === String(total),
      );

  // Finding #30 (dogfood): a skipped task (a `skip` stamps the terminal `cancelled` status) must
  // NOT read like pending — it is segregated into a collapsed "Skipped" sub-section, and reads
  // muted/struck just like `done`.
  it("segregates skipped tasks into a collapsed 'Skipped' section", async () => {
    installBackend(mkPhaseDetail(["done", "cancelled", "cancelled"]));
    await openTasksTab();

    // The done task renders in the phase's ACTIVE list (not inside any <details>).
    const doneTitle = await screen.findByText("Task 0 (done)");
    expect(doneTitle.closest("details")).toBeNull();

    // The skipped tasks are SEGREGATED into the collapsed "Skipped" <details> — not the active list.
    const skippedTitle = screen.getByText("Task 1 (cancelled)");
    const skippedSection = skippedTitle.closest("details");
    expect(skippedSection).not.toBeNull();
    // Collapsed by default, labelled "Skipped", with a count of 2.
    expect((skippedSection as HTMLDetailsElement).open).toBe(false);
    expect(within(skippedSection as HTMLElement).getByText("Skipped")).toBeTruthy();
    expect(within(skippedSection as HTMLElement).getByText("2")).toBeTruthy();

    // A skipped task reads muted/struck (resolved style), the same treatment `done` gets — not
    // the plain `text-foreground` of an open task.
    expect(skippedTitle.className).toMatch(/line-through/);
    expect(doneTitle.className).toMatch(/line-through/);
  });

  // Finding #34 (dogfood, refine of #30): PROGRESS excludes skipped tasks from BOTH numerator and
  // denominator — progress = done / (total − skipped). A phase with 3 done + 3 skip reads 3/3
  // (100%), NOT 6/6; the roll-up header follows the same formula.
  it("progress excludes skipped tasks — all-active-done + skips reads done/active = 100%", async () => {
    installBackend(mkPhaseDetail(["done", "done", "done", "cancelled", "cancelled", "cancelled"]));
    await openTasksTab();
    await screen.findByText("Task 0 (done)");

    // Both the PhaseCard and the roll-up header bars read 3/3 (NOT 6/6) — skips are excluded.
    expect(hasBar(3, 3)).toBe(true);
    expect(hasBar(6, 6)).toBe(false);
    // The counters read 3/3 (phase card) and the header shows 100%.
    expect(screen.getAllByText("3/3").length).toBeGreaterThan(0);
    expect(screen.getByText("100%")).toBeTruthy();
  });

  it("progress excludes skipped tasks — 0 done + pending + skips reads 0/active = 0%", async () => {
    // 0 done + 3 open (todo) + 2 skip → active = 3, done = 0 → 0/3 (0%), NOT 2/5 (40%).
    installBackend(mkPhaseDetail(["todo", "todo", "todo", "cancelled", "cancelled"]));
    await openTasksTab();
    await screen.findByText("Task 0 (todo)");

    expect(hasBar(0, 3)).toBe(true);
    expect(hasBar(2, 5)).toBe(false);
    expect(screen.getAllByText("0/3").length).toBeGreaterThan(0);
    expect(screen.getByText("0%")).toBeTruthy();
  });

  it("a fully-skipped phase reads complete/100% (no divide-by-zero / NaN)", async () => {
    // EDGE CASE: every task skipped → active = 0 → treated as complete/100%, rendered as a full
    // bar (done === total === 1), never NaN.
    installBackend(mkPhaseDetail(["cancelled", "cancelled", "cancelled"]));
    const { container } = renderDetail();
    await screen.findByRole("heading", { name: "The detailed spec" });
    fireEvent.click(screen.getByRole("tab", { name: /Tasks/ }));
    await screen.findByText("Task 0 (cancelled)");

    // The guard turns the empty active set into a full bar and 100% — no NaN anywhere.
    expect(hasBar(1, 1)).toBe(true);
    expect(screen.getByText("100%")).toBeTruthy();
    expect(container.innerHTML).not.toMatch(/NaN/);
  });

  it("the roll-up header follows the same done/(total − skipped) formula across phases", async () => {
    const base = detailFixture();
    // Phase 1: 1 done + 1 skip → 1/1 (100%). Phase 2: 1 open + 1 skip → 0/1 (0%).
    // Spec roll-up: done = 1, active = 2 (skips excluded) → 1/2 (50%), NOT 2/4.
    const tasks = [
      { ...base.tasks[0], id: "a1", title: "P1 done", phase: 1, status: "done" },
      { ...base.tasks[0], id: "a2", title: "P1 skip", phase: 1, status: "cancelled" },
      { ...base.tasks[0], id: "b1", title: "P2 open", phase: 2, status: "todo" },
      { ...base.tasks[0], id: "b2", title: "P2 skip", phase: 2, status: "cancelled" },
    ];
    installBackend({ ...base, tasks, dependencies: [] });
    await openTasksTab();
    await screen.findByText("P1 done");

    // Header roll-up: 1/2 → 50% (skips excluded from BOTH numerator and denominator).
    expect(hasBar(1, 2)).toBe(true);
    expect(hasBar(2, 4)).toBe(false);
    expect(screen.getByText("50%")).toBeTruthy();
    // Phase bars: phase 1 = 1/1, phase 2 = 0/1.
    expect(hasBar(1, 1)).toBe(true);
    expect(hasBar(0, 1)).toBe(true);
  });

  it("mounts the comments-only Review tab with an Open review button (no diff viewer, PDR-0028)", async () => {
    renderDetail();
    await screen.findByRole("heading", { name: "The detailed spec" });
    fireEvent.click(screen.getByRole("tab", { name: /Review/ }));
    // The Review tab is now the COMMENTS surface + an "Open review" button that opens the
    // full-screen review page (`/work/$specId/review`) — NOT the diff viewer, NOT the old
    // "grafts here" placeholder. The heavy viewer (diff/tree) lives only on that page.
    expect(screen.queryByText(/grafts here/)).toBeNull();
    expect(await screen.findByTestId("open-review")).toBeTruthy();
    // No diff viewer chrome mounts in the tab.
    expect(screen.queryByTestId("diff-scroll")).toBeNull();
    expect(screen.queryByRole("tablist", { name: "Repositories" })).toBeNull();
  });

  it("consumes the workspace link in the sidebar (repos + workspace derived from the anchor)", async () => {
    renderDetail();
    await screen.findByRole("heading", { name: "The detailed spec" });
    // The derived workspace + repo appear in the context sidebar.
    expect(screen.getByText("spec-7-worktree")).toBeTruthy();
    expect(screen.getByText("core")).toBeTruthy();
  });

  it("uses design tokens only (no hardcoded colour)", async () => {
    const { container } = renderDetail();
    await screen.findByRole("heading", { name: "The detailed spec" });
    expect(container.innerHTML).not.toMatch(/#[0-9a-fA-F]{3,6}\b|rgba?\(/);
  });

  // T9 (v2.4, temps réel) — a MCP mutation on the Spec broadcasts `specs://changed`; the PRD
  // detail must re-pull `spec_detail` off that push (NO reload / poll) so sections + tasks
  // refresh. Here a `spec_update_section` MCP call changed the problem statement.
  it("re-pulls the PRD detail on a `specs://changed` push (MCP mutation → live refresh)", async () => {
    let live: SpecDetail = detailFixture();
    mockIPC((cmd) => {
      switch (cmd) {
        case "list_projects":
          return [];
        case "list_workspaces":
          return [];
        case "spec_detail":
          return live;
        case "workspace_timeline":
          return { workspace_id: "w1", entries: [] };
        case "spec_activity":
          return { spec_id: "s1", entries: [] };
        case "workspace_runtime_snapshot":
          return {
            workspace: { id: "w1", name: "wt", path: "/tmp" },
            terminals: [],
            agent_sessions: [],
            services: [],
          };
        default:
          return undefined;
      }
    });
    renderDetail();
    expect(await screen.findByText("The problem statement.")).toBeTruthy();

    // A MCP `spec_update_section` rewrote the problem section.
    const base = detailFixture();
    live = { ...base, revision: { ...base.revision, problem: "A freshly rewritten problem." } };
    await emit("specs://changed");

    // The detail re-pulled off the push: the new section content is shown.
    await waitFor(() => expect(screen.getByText("A freshly rewritten problem.")).toBeTruthy());
    expect(screen.queryByText("The problem statement.")).toBeNull();
  });
});
