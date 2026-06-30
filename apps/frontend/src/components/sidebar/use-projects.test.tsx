import { act, renderHook, waitFor } from "@testing-library/react";
import { mockIPC, emit } from "@/bridge/test-harness";
import { beforeEach, describe, expect, it } from "vitest";

import { useProjects, type ProjectRecord, type WorkspaceRecord } from "./use-projects";

/**
 * A fake backend mirroring the project/workspace CRUD relevant to the collapsed
 * persistence: it serves `list_projects` / `list_workspaces` from seeded rows and
 * records every `set_project_collapsed` / `set_workspace_collapsed` call, applying
 * it so a re-list would reflect the persisted flag (as the real backend does).
 */
interface FakeBackend {
  projects: ProjectRecord[];
  workspaces: Record<string, WorkspaceRecord[]>;
  setProjectCollapsed: { id: string; collapsed: boolean }[];
  setWorkspaceCollapsed: { id: string; collapsed: boolean }[];
  /** Every `projects_reorder` call's id sequence, in order received. */
  projectsReorder: string[][];
}

function project(id: string, collapsed = false): ProjectRecord {
  return {
    id,
    name: id.toUpperCase(),
    collapsed,
    created_at: 0,
    updated_at: 0,
    resume_agent_sessions: false,
  };
}

function workspace(id: string, projectId: string, collapsed = false): WorkspaceRecord {
  return {
    id,
    project_id: projectId,
    name: id,
    path: `/${id}`,
    branch: null,
    is_root: true,
    collapsed,
    created_at: 0,
    updated_at: 0,
  };
}

function installBackend(
  projects: ProjectRecord[],
  workspaces: Record<string, WorkspaceRecord[]>,
): FakeBackend {
  const backend: FakeBackend = {
    projects: projects.map((p) => ({ ...p })),
    workspaces: Object.fromEntries(
      Object.entries(workspaces).map(([k, v]) => [k, v.map((w) => ({ ...w }))]),
    ),
    setProjectCollapsed: [],
    setWorkspaceCollapsed: [],
    projectsReorder: [],
  };

  // `shouldMockEvents: true` makes `emit`/`listen` work in tests, so a test can fire
  // the backend `workspaces://changed` event the hook's listener consumes.
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "list_projects":
          return backend.projects;
        case "list_workspaces":
          return backend.workspaces[a.projectId as string] ?? [];
        case "set_project_collapsed": {
          const id = a.id as string;
          const collapsed = a.collapsed as boolean;
          backend.setProjectCollapsed.push({ id, collapsed });
          const row = backend.projects.find((p) => p.id === id);
          if (row) row.collapsed = collapsed;
          return null;
        }
        case "set_workspace_collapsed": {
          const id = a.id as string;
          const collapsed = a.collapsed as boolean;
          backend.setWorkspaceCollapsed.push({ id, collapsed });
          for (const list of Object.values(backend.workspaces)) {
            const row = list.find((w) => w.id === id);
            if (row) row.collapsed = collapsed;
          }
          return null;
        }
        case "projects_reorder": {
          const ids = a.ids as string[];
          backend.projectsReorder.push([...ids]);
          // Reflect the new order so a re-list (e.g. `workspaces://changed`) is
          // authoritative, as the real backend's `order` column would be.
          backend.projects.sort((x, y) => ids.indexOf(x.id) - ids.indexOf(y.id));
          return null;
        }
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
  return backend;
}

describe("useProjects collapsed persistence", () => {
  beforeEach(() => {
    // A fresh empty backend per test (overridden by installBackend below).
    installBackend([], {});
  });

  it("loads the persisted `collapsed` flag onto the project + workspace tree", async () => {
    installBackend([project("p1", true)], { p1: [workspace("w1", "p1", true)] });
    const { result } = renderHook(() => useProjects());
    await waitFor(() => expect(result.current.loading).toBe(false));

    expect(result.current.projects).toHaveLength(1);
    expect(result.current.projects[0].project.collapsed).toBe(true);
    expect(result.current.projects[0].workspaces[0].collapsed).toBe(true);
  });

  it("setProjectCollapsed optimistically updates the tree AND invokes the command", async () => {
    const backend = installBackend([project("p1", false)], { p1: [workspace("w1", "p1")] });
    const { result } = renderHook(() => useProjects());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.projects[0].project.collapsed).toBe(false);

    await act(async () => {
      await result.current.setProjectCollapsed("p1", true);
    });

    // Optimistic local update is reflected immediately on the tree...
    expect(result.current.projects[0].project.collapsed).toBe(true);
    // ...and the backend command was invoked with the new flag.
    expect(backend.setProjectCollapsed).toContainEqual({ id: "p1", collapsed: true });
  });

  it("setWorkspaceCollapsed optimistically updates the tree AND invokes the command", async () => {
    const backend = installBackend([project("p1", false)], {
      p1: [workspace("w1", "p1", false), workspace("w2", "p1", false)],
    });
    const { result } = renderHook(() => useProjects());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.setWorkspaceCollapsed("w2", true);
    });

    const ws2 = result.current.projects[0].workspaces.find((w) => w.id === "w2");
    const ws1 = result.current.projects[0].workspaces.find((w) => w.id === "w1");
    expect(ws2?.collapsed).toBe(true);
    expect(ws1?.collapsed).toBe(false); // a sibling is untouched
    expect(backend.setWorkspaceCollapsed).toContainEqual({ id: "w2", collapsed: true });
  });
});

describe("useProjects reorderProjects (FEEDBACK #11)", () => {
  beforeEach(() => {
    installBackend([], {});
  });

  it("optimistically reorders the tree AND invokes projects_reorder with the new ids", async () => {
    const backend = installBackend([project("p1"), project("p2"), project("p3")], {
      p1: [workspace("w1", "p1")],
      p2: [workspace("w2", "p2")],
      p3: [workspace("w3", "p3")],
    });
    const { result } = renderHook(() => useProjects());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.projects.map((t) => t.project.id)).toEqual(["p1", "p2", "p3"]);

    // Drag p3 to the front.
    await act(async () => {
      await result.current.reorderProjects(["p3", "p1", "p2"]);
    });

    // Optimistic local reorder is reflected immediately on the tree...
    expect(result.current.projects.map((t) => t.project.id)).toEqual(["p3", "p1", "p2"]);
    // ...and the backend command was invoked with the new id order.
    expect(backend.projectsReorder).toEqual([["p3", "p1", "p2"]]);
  });

  it("keeps each project's workspaces attached to it through the reorder", async () => {
    installBackend([project("p1"), project("p2")], {
      p1: [workspace("w1", "p1")],
      p2: [workspace("w2", "p2")],
    });
    const { result } = renderHook(() => useProjects());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.reorderProjects(["p2", "p1"]);
    });

    const [first, second] = result.current.projects;
    expect(first.project.id).toBe("p2");
    expect(first.workspaces.map((w) => w.id)).toEqual(["w2"]);
    expect(second.project.id).toBe("p1");
    expect(second.workspaces.map((w) => w.id)).toEqual(["w1"]);
  });
});

describe("useProjects live refresh on workspaces://changed (review 01KV9611923NKX3JPR5V6MN44F)", () => {
  beforeEach(() => {
    installBackend([], {});
  });

  it("re-pulls the tree when the backend signals a workspace mutation (MCP-driven add)", async () => {
    // The finding: an agent adds a workspace over MCP. The UI never invoked the add,
    // so it never optimistically folded it in — the sidebar updates ONLY via the
    // backend `workspaces://changed` event. Simulate the MCP add by mutating the
    // fake backend WITHOUT going through the hook's own mutators, then firing the
    // event the backend would emit, and assert the hook re-listed and shows it.
    const backend = installBackend([project("p1")], { p1: [workspace("root", "p1")] });
    const { result } = renderHook(() => useProjects());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.projects[0].workspaces).toHaveLength(1);

    // An out-of-band mutation (the MCP `workspace_add`/`create_workspace` path): the
    // row lands in the DB the hook re-lists from, but no UI invoke happened.
    backend.workspaces.p1.push(workspace("feat", "p1"));

    await act(async () => {
      await emit("workspaces://changed");
    });

    await waitFor(() => expect(result.current.projects[0].workspaces).toHaveLength(2));
    expect(result.current.projects[0].workspaces.map((w) => w.id)).toEqual(["root", "feat"]);
  });

  it("reflects an out-of-band project add (and removal) via the event", async () => {
    // The same refresh path covers project create/delete too: a whole new project
    // appearing (or a deleted one vanishing) is reflected by re-listing on the event.
    const backend = installBackend([project("p1")], { p1: [workspace("root", "p1")] });
    const { result } = renderHook(() => useProjects());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.projects).toHaveLength(1);

    backend.projects.push(project("p2"));
    backend.workspaces.p2 = [workspace("root2", "p2")];
    await act(async () => {
      await emit("workspaces://changed");
    });
    await waitFor(() => expect(result.current.projects).toHaveLength(2));

    // And a delete: the project is gone from the backend; the event re-lists it away.
    backend.projects = backend.projects.filter((p) => p.id !== "p1");
    await act(async () => {
      await emit("workspaces://changed");
    });
    await waitFor(() => expect(result.current.projects.map((t) => t.project.id)).toEqual(["p2"]));
  });
});
