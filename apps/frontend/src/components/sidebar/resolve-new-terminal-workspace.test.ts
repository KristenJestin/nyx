import { describe, expect, it } from "vitest";

import { resolveNewTerminalWorkspace } from "./resolve-new-terminal-workspace";
import type { ProjectTree, ProjectRecord, WorkspaceRecord } from "./use-projects";
import type { TerminalRecord } from "./use-terminals";

/**
 * Unit tests for the FEEDBACK #27 resolution: a Ctrl+T "new terminal" inherits
 * the ACTIVE terminal's workspace, with a LOOSE fallback. The resolver is a PURE
 * function (no React), so it is exercised directly here — the same "extract pure,
 * test it" pattern as terminal-geometry.ts. The manager wires a non-null result
 * to create+attach (workspace terminal) and a null result to a loose `create()`.
 */

function terminal(id: string, workspace_id?: string | null): TerminalRecord {
  return {
    id,
    cwd: "/tmp",
    label: null,
    scrollback: "",
    status: "alive",
    order_index: 0,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
    workspace_id: workspace_id ?? null,
  };
}

function workspace(id: string, path: string): WorkspaceRecord {
  return {
    id,
    project_id: "p1",
    name: id,
    path,
    branch: null,
    is_root: false,
    collapsed: false,
    created_at: 0,
    updated_at: 0,
  };
}

function tree(workspaces: WorkspaceRecord[]): ProjectTree {
  const project: ProjectRecord = {
    id: "p1",
    name: "proj",
    collapsed: false,
    created_at: 0,
    updated_at: 0,
    resume_agent_sessions: false,
  };
  return { project, workspaces };
}

describe("resolveNewTerminalWorkspace", () => {
  it("inherits the workspace (path + id) of an active terminal bound to one", () => {
    const ws = workspace("w1", "/home/u/repo");
    const terminals = [terminal("t1", "w1"), terminal("t2")];
    const projects = [tree([ws])];

    expect(resolveNewTerminalWorkspace("t1", terminals, projects)).toEqual({
      path: "/home/u/repo",
      workspaceId: "w1",
    });
  });

  it("falls back (null → loose) when the active terminal is loose", () => {
    const terminals = [terminal("t1"), terminal("t2", "w1")];
    const projects = [tree([workspace("w1", "/home/u/repo")])];

    expect(resolveNewTerminalWorkspace("t1", terminals, projects)).toBeNull();
  });

  it("falls back (null → loose) when there is no active terminal", () => {
    const terminals = [terminal("t1", "w1")];
    const projects = [tree([workspace("w1", "/home/u/repo")])];

    expect(resolveNewTerminalWorkspace(null, terminals, projects)).toBeNull();
  });

  it("falls back (null → loose) when activeId points at no known terminal", () => {
    const terminals = [terminal("t1", "w1")];
    const projects = [tree([workspace("w1", "/home/u/repo")])];

    expect(resolveNewTerminalWorkspace("gone", terminals, projects)).toBeNull();
  });

  it("falls back (null → loose) when the bound workspace is unknown (stale binding)", () => {
    const terminals = [terminal("t1", "w-deleted")];
    const projects = [tree([workspace("w1", "/home/u/repo")])];

    expect(resolveNewTerminalWorkspace("t1", terminals, projects)).toBeNull();
  });

  it("resolves the workspace across multiple projects", () => {
    const terminals = [terminal("t1", "w2")];
    const projects = [
      tree([workspace("w1", "/a")]),
      { ...tree([workspace("w2", "/b/nested")]), project: { ...tree([]).project, id: "p2" } },
    ];

    expect(resolveNewTerminalWorkspace("t1", terminals, projects)).toEqual({
      path: "/b/nested",
      workspaceId: "w2",
    });
  });
});
