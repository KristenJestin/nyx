import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { RouterProvider } from "@tanstack/react-router";
import { beforeEach, describe, expect, it } from "vitest";

import { mockIPC } from "@/bridge/test-harness";
import { createAppRouter } from "@/router";
import { createQueryWrapper } from "@/lib/test-query";
import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

import type { SpecDetail } from "./types";

/**
 * DEEP-LINKING A SPEC ENTERS ITS OWNER PROJECT (PRD fix 2, second half).
 *
 * Landing on a Spec by URL — a route restored at relaunch, a link out of an agent message, a
 * back/forward between two Specs — used to show that Spec while the rail still highlighted whatever
 * project was entered before. Everything project-scoped around the page then read the wrong key.
 *
 * The suite mounts the app's REAL router (`createAppRouter`, so the REAL rail from `<BaseLayout>` is
 * on screen under the real `<ActiveProjectProvider>`) and reads the rail's own `aria-current` — the
 * user-visible answer to "which project am I in" — while checking the URL never moves: the sync is
 * one-way (route → selection), never a redirect, which is what keeps it loop-free.
 */

function project(id: string, name: string): ProjectRecord {
  return { id, name, collapsed: false, created_at: 0, updated_at: 0, resume_agent_sessions: false };
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

/** A `spec_detail` for `specId` OWNED by `projectId` (the field the sync reads). */
function detailFixture(specId: string, projectId: string, projectName: string): SpecDetail {
  return {
    spec: {
      id: specId,
      project_id: projectId,
      workspace_id: `ws-${projectId}`,
      title: `Spec of ${projectName}`,
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
    },
    revision: {
      id: `r-${specId}`,
      spec_id: specId,
      revision_number: 1,
      revision_status: "active",
      title: `Spec of ${projectName}`,
      problem: "",
      solution: "",
      implementation_decisions: "",
      testing_decisions: "",
      user_stories: "",
      out_of_scope: "",
      change_reason: null,
      created_at: 0,
    },
    tasks: [],
    dependencies: [],
    workspace_context: {
      spec_id: specId,
      workspace: workspace(projectId),
      project: { id: projectId, name: projectName, key: projectName.slice(0, 3).toUpperCase() },
      repos: [],
    },
  } as unknown as SpecDetail;
}

/** `s1` belongs to Alpha, `s2` to Beta; `s9` does not exist (the backend rejects it). */
const DETAILS: Record<string, SpecDetail> = {
  s1: detailFixture("s1", "p1", "Alpha"),
  s2: detailFixture("s2", "p2", "Beta"),
};

/**
 * The backend. `treeGate`, when given, HOLDS the two project-tree reads (`list_projects` +
 * `list_workspaces`) until it settles — the ORDERING seam: in jsdom the tree normally wins the
 * race against `spec_detail`, so the reverse order (the real one whenever the tree's two IPC hops
 * are slower than the detail's single one) can only be exercised by forcing it.
 */
function installBackend(treeGate?: Promise<void>) {
  /** `value`, held back until the gate releases when there is one. */
  const tree = <T,>(value: T): T | Promise<T> => (treeGate ? treeGate.then(() => value) : value);
  mockIPC((cmd, args) => {
    const a = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "list_projects":
        return tree([project("p1", "Alpha"), project("p2", "Beta")]);
      case "list_workspaces":
        return tree([workspace(a.projectId as string)]);
      case "spec_detail": {
        const found = DETAILS[a.specId as string];
        if (!found) throw new Error("unknown spec");
        return found;
      }
      case "workspace_runtime_snapshot":
        return {
          workspace: { id: a.workspaceId as string, name: "root", path: "/" },
          terminals: [],
          agent_sessions: [],
          services: [],
        };
      case "spec_activity":
        return { spec_id: a.specId as string, entries: [] };
      case "workspace_timeline":
        return { workspace_id: a.workspaceId as string, entries: [] };
      // The prototype workbench: NO prototype → its existing empty state, no iframe in jsdom.
      case "proto_tree":
        return null;
      default:
        return undefined;
    }
  });
}

/** Mount the real app router at `entry`. Returns the router so the URL can be read/driven. */
function mountAt(entry: string) {
  const router = createAppRouter([entry]);
  const { wrapper } = createQueryWrapper();
  return { ...render(<RouterProvider router={router} />, { wrapper }), router };
}

type AppRouter = ReturnType<typeof createAppRouter>;

/**
 * The THREE Spec-owned routes — detail, review, prototype — as the table the route-agnostic cases
 * run over, so a behaviour the testingDecisions ask for on "detail/review/prototype" cannot end up
 * pinned on one of them only. Each entry carries its own `go` thunk rather than a bare path union
 * because TanStack types `to` against the route tree (a union of route paths does not typecheck).
 */
const SPEC_ROUTES = [
  {
    label: "DETAIL",
    path: (specId: string) => `/work/${specId}`,
    go: (router: AppRouter, specId: string) =>
      router.navigate({ to: "/work/$specId", params: { specId } }),
  },
  {
    label: "REVIEW",
    path: (specId: string) => `/work/${specId}/review`,
    go: (router: AppRouter, specId: string) =>
      router.navigate({ to: "/work/$specId/review", params: { specId } }),
  },
  {
    label: "PROTOTYPE",
    path: (specId: string) => `/work/${specId}/prototype`,
    go: (router: AppRouter, specId: string) =>
      router.navigate({ to: "/work/$specId/prototype", params: { specId }, search: {} }),
  },
] as const;

/** The rail tile of `name`, once the project list has landed. */
function tile(name: string): Promise<HTMLElement> {
  return screen.findByRole("button", { name: `Project ${name}` });
}

/** Wait until the rail marks `name` as the entered project (and nothing else). */
async function expectEntered(name: string, other: string) {
  await waitFor(async () => expect((await tile(name)).getAttribute("aria-current")).toBe("true"));
  expect((await tile(other)).getAttribute("aria-current")).not.toBe("true");
}

describe("Deep-linked Specs enter their owner project", () => {
  beforeEach(() => {
    // The rail restores the LAST entered project from localStorage — pin it to Alpha so every test
    // starts in the SAME project and a move to Beta can only come from the Spec being opened.
    window.localStorage.clear();
    window.localStorage.setItem("nyx.ui.lastProjectId", "p1");
    installBackend();
  });

  it("opening a Spec DETAIL of another project enters that project, URL untouched", async () => {
    const { router } = mountAt("/work/s2");
    expect(await screen.findByRole("heading", { name: "Spec of Beta" })).toBeTruthy();

    await expectEntered("Beta", "Alpha");
    // The deep-link KEEPS its route — the sync never redirects (hence no navigation loop).
    expect(router.state.location.pathname).toBe("/work/s2");
  });

  it("opening a Spec REVIEW of another project enters that project, URL untouched", async () => {
    const { router } = mountAt("/work/s2/review");

    await expectEntered("Beta", "Alpha");
    expect(router.state.location.pathname).toBe("/work/s2/review");
  });

  it("opening a Spec PROTOTYPE of another project enters that project, URL untouched", async () => {
    const { router } = mountAt("/work/s2/prototype");

    await expectEntered("Beta", "Alpha");
    expect(router.state.location.pathname).toBe("/work/s2/prototype");
  });

  /**
   * THE SPEC LANDS BEFORE THE PROJECT TREE — the order the app really has whenever the tree's two
   * IPC hops (`list_projects`, then `list_workspaces` per project) are slower than the detail's
   * single one, which jsdom does not otherwise produce.
   *
   * What used to break: the shell reads an EMPTY project list as "there is no project" and resets
   * the entered project to `null` — persisted hint included — so the selection the Spec had just
   * made was wiped before the tree even arrived, and the once-per-Spec sync never fired again. The
   * user landed on the right Spec inside the WRONG project. The tree now keeps quiet until it has
   * loaded, so the deep-linked owner survives the empty window.
   */
  it("keeps the deep-linked owner when the Spec resolves BEFORE the project tree", async () => {
    let releaseTree = () => {};
    installBackend(new Promise<void>((resolve) => (releaseTree = resolve)));

    const { router } = mountAt("/work/s2");
    // The Spec is on screen while the rail still has NOTHING to show: the sync has already spoken.
    expect(await screen.findByRole("heading", { name: "Spec of Beta" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Project Alpha" })).toBeNull();

    releaseTree();

    // The tree lands on a selection that is already Beta's and leaves it alone — and, as on every
    // other deep-link, the URL never moves.
    await expectEntered("Beta", "Alpha");
    expect(router.state.location.pathname).toBe("/work/s2");
  });

  /**
   * BACK/FORWARD ON EACH OF THE THREE SPEC ROUTES. A history entry carries the Spec, and the Spec
   * carries the project — so walking the stack backwards and forwards must re-enter the OWNER
   * project each way. Run over `SPEC_ROUTES` because the testingDecisions ask for this on detail,
   * review AND prototype: the three pages call the same hook today, and the table is what keeps
   * that true (a page that stops calling it fails here, not silently in the app).
   */
  it.each(SPEC_ROUTES)(
    "back/forward between two projects' Specs re-syncs the rail each way ($label)",
    async ({ path, go }) => {
      const { router } = mountAt(path("s1"));
      await expectEntered("Alpha", "Beta");

      await go(router, "s2");
      await waitFor(() => expect(router.state.location.pathname).toBe(path("s2")));
      await expectEntered("Beta", "Alpha");

      // BACK to Alpha's Spec: the rail follows the route backwards too…
      router.history.back();
      await waitFor(() => expect(router.state.location.pathname).toBe(path("s1")));
      await expectEntered("Alpha", "Beta");

      // …and FORWARD again.
      router.history.forward();
      await waitFor(() => expect(router.state.location.pathname).toBe(path("s2")));
      await expectEntered("Beta", "Alpha");
    },
  );

  /**
   * THE TWO HALVES OF FIX 2 MEETING (the regression this suite exists to hold): the rail ENTERS a
   * project and routes to `/`, while a Spec page ENTERS its owner project. On a real Spec page both
   * are live at once, and a sync that watched the SELECTION would undo the rail click mid-flight —
   * the tile would light up, snap back, and the user would still be in the Spec's project.
   */
  it("clicking another project FROM a real Spec page wins over the page's own sync", async () => {
    const { router } = mountAt("/work/s2");
    await expectEntered("Beta", "Alpha");

    // Leave Beta's Spec for Alpha, from the rail.
    fireEvent.click(await tile("Alpha"));

    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    await expectEntered("Alpha", "Beta");
  });

  it("an UNKNOWN Spec keeps the existing error state and does NOT move the entered project", async () => {
    const { router } = mountAt("/work/s9");
    // The page's own, unchanged error state…
    expect(await screen.findByText("This Spec could not be loaded.")).toBeTruthy();
    // …and the rail is still where it was: a failed read is not an authority on anything.
    await expectEntered("Alpha", "Beta");
    expect(router.state.location.pathname).toBe("/work/s9");
  });
});
