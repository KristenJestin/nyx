import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";
import { saveScopeKind } from "@/components/terminals/ui-persistence";
import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

import type { SpecListEntry } from "./types";

/**
 * THE BOARD IN THE CROSS-PROJECT SCOPE — the SAME page, re-parameterised.
 *
 * `/work` is one board in two scopes: the entered project's Specs, or every project's. What is
 * asserted here is that switching scope changes exactly TWO observable things — WHICH Specs are
 * listed, and (through their key) which projects they belong to — and NOTHING else: same columns,
 * same card, same filters, same order, because it is the same component.
 *
 * Three things this file pins beyond the list itself:
 *
 *  1. the read is made ONCE for the whole scope, with NO project filter — never a call per
 *     project (the fan-out the home's read model exists to avoid);
 *  2. the `activeProjectId === null` empty state ("Enter a project…") CANNOT fire in the
 *     cross-project scope — that `null` means "no project is loaded", and the scope has no
 *     project id at all;
 *  3. in the cross-project scope every card AND every table row wears its project's
 *     `<ProjectMark>`, beside the key it completes — and neither does in the project scope.
 *
 * That third point is a REVERSAL of an earlier decision of this file's ("the key already names the
 * project, so no mark"). The reasoning was defensible and the user overruled it on the real board
 * (« je suis en all dans works, j'ai aucun sign du projet en question sur les card ? »): a key is
 * READ, a mark is SEEN. Both views are asserted, because one view marking and the other not is the
 * inconsistency the reversal exists to remove.
 */

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
    // PDR-0017: uncategorized is the steady state, and the field is REQUIRED on the wire shape.
    category_id: null,
    project_key: "ALPHA",
    project: { id: "p1", name: "Alpha", accent: null, icon: null, image_path: null },
    tasks_done: 0,
    tasks_total: 0,
    blocked_by_count: 0,
    unblocks_count: 0,
    ...overrides,
  };
}

/** Alpha's one Spec — what the PROJECT scope shows. */
const ALPHA_SPECS: SpecListEntry[] = [
  specEntry({ id: "s1", number: 1, title: "Alpha work", project_id: "p1", project_key: "ALPHA" }),
];
/** …and Beta's, which only the CROSS-PROJECT scope adds. */
const ALL_SPECS: SpecListEntry[] = [
  ...ALPHA_SPECS,
  specEntry({
    id: "s2",
    number: 1,
    title: "Beta work",
    project_id: "p2",
    project_key: "BETA",
    project: { id: "p2", name: "Beta", accent: "#f97316", icon: null, image_path: null },
  }),
];

function project(id: string, name: string): ProjectRecord {
  return { id, name, key: "P", collapsed: false, created_at: 0, updated_at: 0, resume_agent_sessions: false };
}

function workspace(projectId: string): WorkspaceRecord {
  return {
    id: `ws-${projectId}`,
    project_id: projectId,
    name: "root",
    path: `/${projectId}`,
    branch: null,
    is_root: true,
    collapsed: false,
    created_at: 0,
    updated_at: 0,
  };
}

/** Every `spec_list` argument object the page asked for, in order. */
let reads: Record<string, unknown>[] = [];

function installBackend() {
  reads = [];
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "list_projects":
          return [project("p1", "Alpha"), project("p2", "Beta")];
        case "list_workspaces":
          return [workspace((a.projectId as string) ?? "p1")];
        case "workspace_runtime_snapshot":
          return {
            workspace: { id: (a.workspaceId as string) ?? "ws-p1", name: "root", path: "/" },
            terminals: [],
            agent_sessions: [],
            services: [],
          };
        case "home_snapshot":
          return { sessions: [], services: [], specs: [], terminals: [] };
        case "spec_list":
          reads.push(a);
          // The backend serves the scope it was ASKED for: an absent `projectId` is every
          // project — the same "absent = all" shape `home_snapshot` takes.
          return a.projectId === undefined
            ? ALL_SPECS
            : ALL_SPECS.filter((s) => s.project_id === a.projectId);
        case "system_scope_detail":
          // The Système singleton is ABSENT on this fixture — the documented `null` answer of a
          // database with no scope. The rail then renders no tile, which is what these cases are
          // about; the catch-all below answers `[]`, and `[]` is not a `{project, workspace, home}`.
          return null;
        default:
          return [];
      }
    },
    { shouldMockEvents: true },
  );
}

function mount(path = "/work") {
  const router = createAppRouter([path]);
  return { ...render(<RouterProvider router={router} />), router };
}

/** The titles the board lists, in DOM order — the one thing the scope is meant to change. */
function listedTitles(): string[] {
  return screen
    .getAllByTestId(/^spec-card-/)
    .map((card) => card.querySelector("h3")?.textContent ?? "");
}

/** The human keys the cards lead with. */
function listedKeys(): string[] {
  return screen
    .getAllByTestId(/^spec-card-/)
    .map((card) => card.querySelector('[data-testid="spec-key"]')?.textContent ?? "");
}

/** The project each card is marked with (`null` when the card wears no mark), in DOM order. */
function listedMarks(): (string | null)[] {
  return screen
    .getAllByTestId(/^spec-card-/)
    .map(
      (card) =>
        card.querySelector('[data-testid="spec-project-mark"]')?.getAttribute("title") ?? null,
    );
}

/** Switch the board to its LIST view — the other half of "both views mark". */
function showListView() {
  fireEvent.click(screen.getByRole("button", { name: "List view" }));
}

/** The `<td>` Key cells of the table view: the key, and the project its mark names. */
function tableKeyCells(): { key: string; mark: string | null }[] {
  return [...document.querySelectorAll("tbody tr")].map((row) => ({
    key: row.querySelector('[data-testid="spec-key"]')?.textContent ?? "",
    mark: row.querySelector('[data-testid="spec-project-mark"]')?.getAttribute("title") ?? null,
  }));
}

describe("The Specs board follows the scope", () => {
  beforeEach(() => {
    saveScopeKind("project");
    installBackend();
  });

  it("lists the entered project's Specs in the project scope", async () => {
    mount();
    await waitFor(() => expect(listedTitles()).toEqual(["Alpha work"]));
    expect(reads).toContainEqual({ projectId: "p1" });
  });

  it("lists EVERY project's Specs in the cross-project scope, in ONE read", async () => {
    saveScopeKind("all");
    mount();
    await waitFor(() => expect(listedTitles()).toEqual(["Alpha work", "Beta work"]));
    // ONE read for the WHOLE scope, with no project filter — and NOT one call per project.
    expect(reads).toContainEqual({});
    expect(reads.filter((r) => r.projectId !== undefined)).toEqual([]);
  });

  /**
   * The reversal, asserted on BOTH views. The mark does not REPLACE the key — the two sit side by
   * side in the same slot, one seen and one read — and it is never a column of its own, so the
   * board keeps the same shape in both scopes.
   */
  it("names each Spec's project TWICE in the cross-project scope: the mark AND the key", async () => {
    saveScopeKind("all");
    mount();
    await waitFor(() => expect(listedKeys()).toEqual(["ALPHA-1", "BETA-1"]));
    expect(listedMarks()).toEqual(["Alpha", "Beta"]);

    // …and the LIST view says exactly the same thing, in its Key cell.
    showListView();
    await waitFor(() =>
      expect(tableKeyCells()).toEqual([
        { key: "ALPHA-1", mark: "Alpha" },
        { key: "BETA-1", mark: "Beta" },
      ]),
    );
    // The table still has its five columns — the mark rode INTO the Key cell, it did not add one.
    expect(document.querySelectorAll("thead th")).toHaveLength(5);
  });

  it("drops the mark in the project scope — there is nothing to tell apart, both views", async () => {
    mount();
    await waitFor(() => expect(listedKeys()).toEqual(["ALPHA-1"]));
    expect(listedMarks()).toEqual([null]);
    expect(screen.queryByTestId("spec-project-mark")).toBeNull();

    showListView();
    await waitFor(() => expect(tableKeyCells()).toEqual([{ key: "ALPHA-1", mark: null }]));
  });

  it("keeps `Enter a project…` for NO PROJECT LOADED, and never in the cross-project scope", async () => {
    // No project at all: the honest "nothing is entered" state, unchanged.
    mockIPC(
      (cmd) => {
        switch (cmd) {
          case "home_snapshot":
            return { sessions: [], services: [], specs: [], terminals: [] };
          case "spec_list":
            return [];
          case "system_scope_detail":
            // The Système singleton is ABSENT on this fixture — the documented `null` answer of a
            // database with no scope. The rail then renders no tile, which is what these cases are
            // about; the catch-all below answers `[]`, and `[]` is not a `{project, workspace, home}`.
            return null;
          default:
            return [];
        }
      },
      { shouldMockEvents: true },
    );
    const { unmount } = mount();
    expect(await screen.findByText("Enter a project to see its Specs.")).toBeTruthy();
    unmount();

    // …and in the cross-project scope that branch is unreachable: the scope carries no project id
    // to misread as "none loaded", so the page states the scope's own emptiness instead.
    saveScopeKind("all");
    mount();
    expect(await screen.findByTestId("board-empty")).toBeTruthy();
    expect(screen.queryByText("Enter a project to see its Specs.")).toBeNull();
  });

  it("changes ONLY the list when the scope flips — live, both ways", async () => {
    saveScopeKind("all");
    mount();
    await waitFor(() => expect(listedTitles()).toEqual(["Alpha work", "Beta work"]));
    // The chrome the two scopes share, captured before the switch.
    const before = screen.getByRole("heading", { name: "Specs" }).textContent;
    const columns = screen.getAllByTestId(/^kanban-column-/).map((c) => c.dataset.testid);

    // Entering a project is the ONE gesture that leaves the mode; it lands on the home, so come
    // back to the board through the rail — the page is the same one either way.
    fireEvent.click(screen.getByRole("button", { name: "Project Alpha" }));
    fireEvent.click(await screen.findByRole("link", { name: "Work" }));

    await waitFor(() => expect(listedTitles()).toEqual(["Alpha work"]));
    // …and NOTHING else moved: same heading, same columns, same card anatomy.
    expect(screen.getByRole("heading", { name: "Specs" }).textContent).toBe(before);
    expect(screen.getAllByTestId(/^kanban-column-/).map((c) => c.dataset.testid)).toEqual(columns);
  });
});
