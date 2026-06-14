import { act, renderHook, waitFor } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
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
}

function project(id: string, collapsed = false): ProjectRecord {
  return { id, name: id.toUpperCase(), collapsed, created_at: 0, updated_at: 0 };
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
  };

  mockIPC((cmd, args) => {
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
      default:
        return null;
    }
  });
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
