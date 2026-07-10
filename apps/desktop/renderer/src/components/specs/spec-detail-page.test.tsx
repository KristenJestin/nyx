import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { emit, mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";

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

function installBackend(detail: SpecDetail = detailFixture()) {
  mockIPC((cmd) => {
    switch (cmd) {
      case "list_projects":
        return [];
      case "list_workspaces":
        return [];
      case "spec_detail":
        return detail;
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
  return render(<RouterProvider router={router} />);
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

  it("renders the Review tab as a placeholder / anchor (no review content)", async () => {
    renderDetail();
    await screen.findByRole("heading", { name: "The detailed spec" });
    fireEvent.click(screen.getByRole("tab", { name: /Review/ }));
    // The placeholder tells the user the review grafts here (v2.5). No diff/findings.
    expect(await screen.findByText(/grafts here/)).toBeTruthy();
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
