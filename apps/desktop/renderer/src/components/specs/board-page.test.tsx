import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { emit, mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";

import type { SpecListEntry } from "./types";

/** A `spec_list` entry fixture. */
function specEntry(overrides: Partial<SpecListEntry>): SpecListEntry {
  return {
    id: "s1",
    project_id: "p1",
    workspace_id: null,
    title: "Untitled spec",
    slug: null,
    number: 1,
    priority: "med",
    status: "in_progress",
    mode: "stepped",
    phases: "[]",
    current_revision_number: 1,
    position: 0,
    created_at: 0,
    updated_at: 0,
    completed_at: null,
    archived_at: null,
    project_key: "NYX",
    tasks_done: 0,
    tasks_total: 0,
    ...overrides,
  };
}

const SPECS: SpecListEntry[] = [
  specEntry({
    id: "s1",
    number: 1,
    title: "Ready spec",
    status: "ready",
    priority: "high",
    tasks_done: 1,
    tasks_total: 3,
  }),
  specEntry({
    id: "s2",
    number: 2,
    title: "In progress spec",
    status: "in_progress",
    priority: "urgent",
    tasks_done: 2,
    tasks_total: 4,
  }),
  specEntry({
    id: "s3",
    number: 3,
    title: "Another in progress",
    status: "in_progress",
    priority: "low",
  }),
];

function installBackend(specs: SpecListEntry[] = SPECS) {
  mockIPC((cmd) => {
    switch (cmd) {
      case "list_projects":
        // The flat `ProjectRecord[]` the shell's `useProjects` expects (it fetches each
        // project's workspaces separately via `list_workspaces`).
        return [
          {
            id: "p1",
            name: "Project",
            key: "NYX",
            collapsed: false,
            created_at: 0,
            updated_at: 0,
            resume_agent_sessions: false,
          },
        ];
      case "list_workspaces":
        return [];
      case "spec_list":
        return specs;
      default:
        return undefined;
    }
  });
}

function renderBoard() {
  const router = createAppRouter(["/work"]);
  return render(<RouterProvider router={router} />);
}

describe("BoardPage", () => {
  beforeEach(() => {
    installBackend();
  });

  it("renders the board header (title + subtitle counts)", async () => {
    renderBoard();
    expect(await screen.findByRole("heading", { name: "Specs" })).toBeTruthy();
    // 3 specs, 2 in_progress.
    expect(await screen.findByText(/3 specs · 2 in progress/)).toBeTruthy();
  });

  it("renders status columns (kanban) with the Specs grouped by status", async () => {
    renderBoard();
    // The status column labels exist (each column mounts an expanded + a collapsed label,
    // so a status label appears more than once — assert it is present at all).
    expect((await screen.findAllByText("Ready", { exact: false })).length).toBeGreaterThan(0);
    expect(screen.getAllByText("In progress", { exact: false }).length).toBeGreaterThan(0);
    // The cards appear under their columns (links to the detail).
    expect(await screen.findByRole("link", { name: /Ready spec/ })).toBeTruthy();
    expect(screen.getByRole("link", { name: /In progress spec/ })).toBeTruthy();
  });

  it("toggles between kanban and list (table) views", async () => {
    renderBoard();
    await screen.findByRole("heading", { name: "Specs" });
    // Switch to the list view — the table header (Key / Title / Status / Priority) appears.
    fireEvent.click(screen.getByRole("button", { name: "List view" }));
    expect(await screen.findByRole("columnheader", { name: "Key" })).toBeTruthy();
    expect(screen.getByRole("columnheader", { name: "Title" })).toBeTruthy();
    // Every Spec is a row in the flat table.
    const table = screen.getByRole("table");
    expect(within(table).getByRole("link", { name: /Ready spec/ })).toBeTruthy();
    expect(within(table).getByRole("link", { name: /In progress spec/ })).toBeTruthy();
    // Switch back to the board.
    fireEvent.click(screen.getByRole("button", { name: "Board view" }));
    await waitFor(() => expect(screen.queryByRole("table")).toBeNull());
  });

  it("filters the board by priority", async () => {
    renderBoard();
    await screen.findByRole("link", { name: /Ready spec/ });
    // Select the URGENT priority facet — only the urgent Spec stays.
    fireEvent.click(screen.getByRole("button", { name: /Urgent/ }));
    await waitFor(() => expect(screen.queryByRole("link", { name: /Ready spec/ })).toBeNull());
    expect(screen.getByRole("link", { name: /In progress spec/ })).toBeTruthy();
  });

  it("uses design tokens only (no hardcoded colour)", async () => {
    const { container } = renderBoard();
    await screen.findByRole("heading", { name: "Specs" });
    expect(container.innerHTML).not.toMatch(/#[0-9a-fA-F]{3,6}\b|rgba?\(/);
  });

  // T9 (v2.4, temps réel) — a MCP spec/task mutation broadcasts `specs://changed`; the board
  // must re-pull `spec_list` off that push (NO reload / poll) so status + counters refresh.
  it("re-pulls the board on a `specs://changed` push (MCP mutation → live refresh)", async () => {
    // Mutable backing: the first `spec_list` returns one in_progress spec with 1/3 tasks;
    // after a "MCP mutation" we swap in the DONE version with 3/3 tasks, then push the topic.
    let live: SpecListEntry[] = [
      specEntry({
        id: "s1",
        number: 1,
        title: "Live spec",
        status: "in_progress",
        tasks_done: 1,
        tasks_total: 3,
      }),
    ];
    mockIPC((cmd) => {
      switch (cmd) {
        case "list_projects":
          return [
            {
              id: "p1",
              name: "Project",
              key: "NYX",
              collapsed: false,
              created_at: 0,
              updated_at: 0,
              resume_agent_sessions: false,
            },
          ];
        case "list_workspaces":
          return [];
        case "spec_list":
          return live;
        default:
          return undefined;
      }
    });
    renderBoard();
    // Initial header: 1 spec, 1 in progress.
    expect(await screen.findByText(/1 specs · 1 in progress/)).toBeTruthy();

    // A MCP `spec_transition` moved the spec to done and completed its tasks.
    live = [
      specEntry({
        id: "s1",
        number: 1,
        title: "Live spec",
        status: "done",
        tasks_done: 3,
        tasks_total: 3,
      }),
    ];
    await emit("specs://changed");

    // The board re-pulled off the push: 0 in progress now (status + counters refreshed).
    await waitFor(() => expect(screen.getByText(/1 specs · 0 in progress/)).toBeTruthy());
  });
});
