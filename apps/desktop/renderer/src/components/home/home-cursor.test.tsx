import { render, screen } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";
import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

import type { HomeProject, HomeSnapshot } from "./types";

/**
 * THE AFFORDANCE OF A CLICKABLE ROW, swept across the FOUR families of the homes.
 *
 * The user reported it on two of them (« contrairement au recent specs, j'ai pas le curseur sur
 * agent ou sur terminals »), but the gap was never a decision: the Spec cards read as clickable
 * because they are `<Link>`s and the browser gives an anchor the hand cursor for free, and the
 * three `<button>` families inherit nothing. Fixing only the two families that were named would
 * leave the third to re-create the same report, so this file asks the same two questions of ALL
 * FOUR — sessions, services, terminals, Spec cards — and it asks them in BOTH scopes, since the
 * two homes are the same components.
 *
 * The two questions are the same affordance seen from the two input devices:
 *   1. THE POINTER — `cursor-pointer`, written explicitly, the Spec card included: relying on the
 *      anchor rule is what made the four disagree in the first place.
 *   2. THE KEYBOARD — a real focusable control plus the app's `focus-visible` RING. The rows were
 *      focusable already, but their only focus signal was the same `bg-muted/60` tint as hover.
 *
 * And the third question is a NON-change: the three button families must STAY buttons. Their
 * navigation goes through `use-home-focus`, which poses the active project AND the deep-link in
 * one act; an anchor would route through the rail's `enterProject` shape and erase the search
 * (review `01M069MWRNR1PEZZW7YTPAHSKK`, pinned by `home-focus.test.tsx`).
 */

const NYX: HomeProject = { id: "p1", name: "nyx", accent: null, icon: null, image_path: null };

const SNAPSHOT: HomeSnapshot = {
  sessions: [
    {
      session_id: "s1",
      terminal_id: "t1",
      agent_kind: "claude_code",
      state: "idle",
      secondary: null,
      label: "renderer",
      cwd: "/nyx",
      started_at: 0,
      workspace: { id: "ws1", name: "nyx-v2", branch: "v2" },
      project: NYX,
    },
  ],
  services: [
    {
      instance_id: "inst-1",
      name: "renderer dev",
      run_state: "running",
      exit_code: null,
      unread: false,
      workspace: { id: "ws1", name: "nyx-v2", branch: "v2" },
      project: NYX,
    },
  ],
  specs: [
    {
      spec_id: "sp1",
      key: "NYX-42",
      title: "Project home & cross-project home",
      status: "in_progress",
      updated_at: 0,
      tasks_done: 1,
      tasks_total: 2,
      phase: null,
      project: NYX,
    },
  ],
  terminals: [
    {
      terminal_id: "t2",
      label: "shell",
      cwd: "/nyx",
      exec_state: "idle",
      exec_state_unread: false,
      workspace: { id: "ws1", name: "nyx-v2", branch: "v2" },
      project: NYX,
    },
  ],
};

function projectRecord(id: string, name: string): ProjectRecord {
  return { id, name, collapsed: false, created_at: 0, updated_at: 0, resume_agent_sessions: false };
}

function workspaceRecord(projectId: string): WorkspaceRecord {
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

function installBackend() {
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "list_projects":
          return [projectRecord("p1", "nyx")];
        case "list_workspaces":
          return [workspaceRecord((a.projectId as string) ?? "p1")];
        case "workspace_runtime_snapshot":
          return {
            workspace: { id: (a.workspaceId as string) ?? "ws-p1", name: "root", path: "/" },
            terminals: [],
            agent_sessions: [],
            services: [],
          };
        case "home_snapshot":
          return SNAPSHOT;
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

/** The FOUR families, by the test id each row names itself with. */
const FAMILIES = [
  ["agent session", "home-session-s1"],
  ["service", "home-service-inst-1"],
  ["terminal", "home-terminal-t2"],
  ["spec card", "home-spec-sp1"],
] as const;

/** The three families whose navigation goes through `use-home-focus` — buttons, never anchors. */
const FOCUS_DRIVEN = ["home-session-s1", "home-service-inst-1", "home-terminal-t2"] as const;

async function mountHome(path: string): Promise<void> {
  const router = createAppRouter([path]);
  render(<RouterProvider router={router} />);
  await screen.findByTestId("home-page");
}

describe.each([
  ["the project home", "/home"],
  ["the cross-project home", "/home?scope=all"],
])("Every clickable row of %s signals that it is one", (_label, path) => {
  beforeEach(() => {
    installBackend();
  });

  it.each(FAMILIES)("shows the hand cursor on a %s row", async (_family, testId) => {
    await mountHome(path);
    expect((await screen.findByTestId(testId)).className).toContain("cursor-pointer");
  });

  it.each(FAMILIES)("is keyboard-reachable and rings on focus — %s row", async (_f, testId) => {
    await mountHome(path);
    const row = await screen.findByTestId(testId);
    // A real control: a `<button>` or an `<a href>` — reachable in the tab order with no
    // hand-rolled `tabindex`, and never a bare `<div onClick>`.
    expect(["BUTTON", "A"]).toContain(row.tagName);
    if (row.tagName === "A") expect(row.getAttribute("href")).toBeTruthy();
    expect(row.getAttribute("tabindex")).toBeNull();
    // …and the focus signal is the app's RING, not the hover tint said twice.
    expect(row.className).toContain("focus-visible:ring-2");
    expect(row.className).toContain("focus-visible:ring-ring");
  });

  it.each(FOCUS_DRIVEN)("keeps %s a <button> — the deep-link path is not an anchor", async (id) => {
    await mountHome(path);
    const row = await screen.findByTestId(id);
    expect(row.tagName).toBe("BUTTON");
    expect(row.getAttribute("href")).toBeNull();
  });
});
