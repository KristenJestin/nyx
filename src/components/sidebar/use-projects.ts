import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type { ExecState } from "./use-terminals";

/**
 * Backend event broadcast on EVERY mutation of the project/workspace tree, whether
 * it originated in a UI `#[tauri::command]` (`create_project`/`create_workspace`/
 * `delete_project`) or in an MCP tool (`workspace_add`/`create_workspace`). Mirrors
 * `bridge::WORKSPACES_CHANGED_EVENT`. The hook re-pulls the whole tree on receipt so
 * an agent-driven mutation (which the UI never invoked, so never optimistically
 * folded in) shows up in the sidebar WITHOUT a manual reload — the command tools'
 * `command://state` analogue for the tree (review 01KV9611923NKX3JPR5V6MN44F).
 */
const WORKSPACES_CHANGED_EVENT = "workspaces://changed";

/**
 * Project / workspace state for the sidebar spine (PRD-2 Phase 2).
 *
 * The sidebar is `projects → workspaces → terminals`. This hook owns the project
 * list and, per project, its workspaces (loaded from the Phase-1 backend
 * commands `list_projects` / `list_workspaces`), plus the create mutations
 * (`create_project` / `create_workspace`) the manual-add flow (ZDZ) drives.
 *
 * Terminals are owned by `useTerminals`; the manager joins the two id-spaces by
 * grouping terminals under their `workspace_id`. This hook is deliberately
 * unaware of terminals — it only describes the project/workspace tree.
 */

/** A `projects` row, mirroring `db::Project` across the IPC boundary. */
export interface ProjectRecord {
  id: string;
  name: string;
  /**
   * Persisted sidebar disclosure state (`false` = open/expanded). The band
   * initializes its `open` from `!collapsed` so the open/closed state survives
   * a restart (`setProjectCollapsed` persists a toggle).
   */
  collapsed: boolean;
  /** Epoch milliseconds. */
  created_at: number;
  /** Epoch milliseconds. */
  updated_at: number;
  /**
   * Per-project opt-in (default `false`) to RESUME this project's terminals'
   * active agent sessions at relaunch (PRD-5 #5): when `true`, nyx injects
   * `claude --resume <id>` into the respawned shell instead of a bare shell.
   * Mirrors `db::Project.resume_agent_sessions`. Default OFF also means closing a
   * terminal with a live session here triggers the close-warning (#6).
   */
  resume_agent_sessions: boolean;
}

/** A `workspaces` row, mirroring `db::Workspace` across the IPC boundary. */
export interface WorkspaceRecord {
  id: string;
  project_id: string;
  name: string;
  /** Canonical, backend-normalized absolute path. */
  path: string;
  branch: string | null;
  is_root: boolean;
  /**
   * Persisted sidebar disclosure state (`false` = open/expanded). Mirrors the
   * project `collapsed`; `setWorkspaceCollapsed` persists a band toggle.
   */
  collapsed: boolean;
  created_at: number;
  updated_at: number;
}

/** Payload returned by `create_project`: the project + its named root workspace. */
export interface ProjectWithRoot {
  project: ProjectRecord;
  root: WorkspaceRecord;
}

/** A project joined with its (root-first) workspaces, as the sidebar renders it. */
export interface ProjectTree {
  project: ProjectRecord;
  workspaces: WorkspaceRecord[];
}

/**
 * A workspace command (structural placeholder until PRD-3 feeds them). Only the
 * `id` + `label` are needed for the sidebar Commandes subsection; the subsection
 * is rendered ONLY when at least one command exists (empty-state polish).
 */
export interface CommandRecord {
  id: string;
  label: string;
  /**
   * The FACTUAL run-state for the command's `<StatusDot>` (the run-state channel,
   * finding 01KV305BGS69RWCSWCAF0KD2SJ). Optional, defaults to `'idle'`. An
   * acknowledge NEVER changes this — it reflects the true last-run outcome.
   */
  state?: ExecState;
  /**
   * The "unseen result" flag (PRD-4 v4): `true` while a finished run's
   * success/error result has not been acknowledged in the UI. Drives the settled
   * BADGE's visibility (the row hides it on acknowledge) while `state` keeps showing
   * the factual outcome. Optional, defaults to `false`.
   */
  unread?: boolean;
}

/** The imperative surface the sidebar + add-flow drive. */
export interface UseProjects {
  /** Projects (with their workspaces), in creation order. */
  projects: ProjectTree[];
  /** True until the initial `list_projects` (+ workspaces) has resolved. */
  loading: boolean;
  /**
   * Create a project at `rootPath` with a named root workspace, prepend-load it
   * into the tree, and return the new project + root workspace. `rootName`
   * defaults (backend-side) to the folder's own name when omitted/blank.
   */
  createProject: (name: string, rootPath: string, rootName?: string) => Promise<ProjectWithRoot>;
  /**
   * Add a (non-root) workspace at `path` to `projectId` and refresh that
   * project's workspaces. Rejects (backend UNIQUE(project_id, path)) when the
   * path already exists in the same project; the caller surfaces the error.
   */
  createWorkspace: (projectId: string, name: string, path: string) => Promise<WorkspaceRecord>;
  /**
   * Rename a project's display `name` (the label shown in its sidebar header).
   * Optimistically reflected, then persisted via the backend `update_project`.
   */
  updateProject: (id: string, name: string) => Promise<void>;
  /**
   * Rename a workspace's display `name` (the path is immutable). Optimistically
   * reflected, then persisted via the backend `rename_workspace`.
   */
  renameWorkspace: (id: string, name: string) => Promise<void>;
  /**
   * Delete a project and its workspaces. Terminals bound to those workspaces are
   * detached (workspace_id → null) backend-side and survive as loose terminals;
   * the project is dropped from the in-memory tree.
   */
  deleteProject: (id: string) => Promise<void>;
  /**
   * Persist a project's `resume_agent_sessions` opt-in (PRD-5 #5). Optimistically
   * reflected on the tree, then persisted via `set_project_resume_agent_sessions`.
   */
  setProjectResumeAgentSessions: (id: string, resume: boolean) => Promise<void>;
  /**
   * Persist a project band's open/closed state. Optimistically reflected on the
   * tree (so the next reload restores the same disclosure), then persisted via
   * the backend `set_project_collapsed`.
   */
  setProjectCollapsed: (id: string, collapsed: boolean) => Promise<void>;
  /**
   * Persist a workspace band's open/closed state. Optimistically reflected on the
   * tree, then persisted via the backend `set_workspace_collapsed`.
   */
  setWorkspaceCollapsed: (id: string, collapsed: boolean) => Promise<void>;
}

/**
 * Load every project together with its workspaces. Each project's workspaces are
 * fetched with `list_workspaces` (root first); the joined trees are returned in
 * project creation order. A single place so both the initial load and an
 * after-mutation refresh share the same shape.
 */
async function loadProjectTrees(): Promise<ProjectTree[]> {
  const projects = await invoke<ProjectRecord[]>("list_projects");
  const trees = await Promise.all(
    projects.map(async (project) => {
      const workspaces = await invoke<WorkspaceRecord[]>("list_workspaces", {
        projectId: project.id,
      });
      return { project, workspaces };
    }),
  );
  return trees;
}

/**
 * `useProjects` — the project/workspace tree behind the sidebar spine.
 *
 * On mount it loads the existing projects + their workspaces. `createProject` /
 * `createWorkspace` invoke the backend and then optimistically fold the result
 * into the in-memory tree (re-listing the affected project's workspaces so the
 * single-root ordering stays authoritative). Empty tree (no projects yet) is a
 * valid state — the sidebar simply shows no project rows.
 */
export function useProjects(): UseProjects {
  const [projects, setProjects] = useState<ProjectTree[]>([]);
  const [loading, setLoading] = useState(true);

  // StrictMode double-mount guard: load exactly once per real mount.
  const bootstrapped = useRef(false);

  useEffect(() => {
    if (bootstrapped.current) return;
    bootstrapped.current = true;
    void (async () => {
      try {
        setProjects(await loadProjectTrees());
      } catch {
        // list_projects failed (transient IPC/DB): leave the tree empty; the
        // next mutation re-lists and recovers.
        setProjects([]);
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  // Re-pull the whole tree whenever the backend signals it changed. This is the
  // single refresh path BOTH the UI's own mutations and the MCP tools' mutations
  // converge on: the UI commands already fold their result in optimistically (so
  // this is a cheap idempotent re-list for them), but an MCP-driven add/delete — one
  // the UI never invoked — is reflected ONLY via this event. Re-listing (rather than
  // applying a delta) keeps the sidebar authoritative against the DB regardless of
  // who mutated. StrictMode-safe: the listener is torn down on cleanup and a late
  // resolve after unmount is unlistened immediately.
  useEffect(() => {
    let torndown = false;
    let unlisten: (() => void) | undefined;
    void listen(WORKSPACES_CHANGED_EVENT, () => {
      if (torndown) return;
      void loadProjectTrees()
        .then((trees) => {
          if (!torndown) setProjects(trees);
        })
        // A transient list failure leaves the current tree; the next event recovers.
        .catch(() => {});
    }).then((un) => {
      if (torndown) {
        void Promise.resolve(un()).catch(() => {});
        return;
      }
      unlisten = un;
    });
    return () => {
      torndown = true;
      if (unlisten) void Promise.resolve(unlisten()).catch(() => {});
    };
  }, []);

  /** Re-list a single project's workspaces and fold them into the tree. */
  const refreshWorkspaces = useCallback(async (projectId: string) => {
    const workspaces = await invoke<WorkspaceRecord[]>("list_workspaces", {
      projectId,
    });
    setProjects((prev) => prev.map((t) => (t.project.id === projectId ? { ...t, workspaces } : t)));
  }, []);

  const createProject = useCallback(async (name: string, rootPath: string, rootName?: string) => {
    const created = await invoke<ProjectWithRoot>("create_project", {
      name,
      rootPath,
      rootName: rootName ?? null,
    });
    setProjects((prev) => [...prev, { project: created.project, workspaces: [created.root] }]);
    return created;
  }, []);

  const createWorkspace = useCallback(
    async (projectId: string, name: string, path: string) => {
      const ws = await invoke<WorkspaceRecord>("create_workspace", {
        projectId,
        name,
        path,
      });
      // Re-list so root-first ordering / any backend defaults are authoritative.
      await refreshWorkspaces(projectId);
      return ws;
    },
    [refreshWorkspaces],
  );

  const updateProject = useCallback(async (id: string, name: string) => {
    // Optimistically reflect the new name so the header repaints immediately,
    // then persist. A failure leaves the optimistic name; the next list corrects.
    setProjects((prev) =>
      prev.map((t) => (t.project.id === id ? { ...t, project: { ...t.project, name } } : t)),
    );
    await invoke("update_project", { id, name }).catch(() => {});
  }, []);

  const deleteProject = useCallback(async (id: string) => {
    // Await the backend FIRST, then drop the project from the tree only on success.
    // `delete_project` REFUSES (Err) when the project still has a running command,
    // so an optimistic-remove-then-swallow would desync the sidebar from the DB —
    // the project would vanish locally while it (and its live process) survive, and
    // its terminals would be detached for nothing. The rejection now PROPAGATES so
    // the caller skips the terminal detach and the confirm modal surfaces the
    // message. The backend detaches bound terminals (workspace_id → null);
    // `useTerminals` reflects that via its own list/auto-attach passes; here we only
    // own the project/workspace tree. The delete sits behind a confirm dialog with a
    // submitting spinner, so awaiting first costs no perceptible snappiness.
    await invoke("delete_project", { id });
    setProjects((prev) => prev.filter((t) => t.project.id !== id));
  }, []);

  const renameWorkspace = useCallback(async (id: string, name: string) => {
    // Optimistically reflect the new workspace name across the tree, then persist.
    setProjects((prev) =>
      prev.map((t) => ({
        ...t,
        workspaces: t.workspaces.map((w) => (w.id === id ? { ...w, name } : w)),
      })),
    );
    await invoke("rename_workspace", { id, name }).catch(() => {});
  }, []);

  const setProjectResumeAgentSessions = useCallback(async (id: string, resume: boolean) => {
    // Optimistically reflect the toggle so the dialog/header repaint immediately,
    // then persist. A failure leaves the optimistic flag; the next list corrects it.
    setProjects((prev) =>
      prev.map((t) =>
        t.project.id === id
          ? { ...t, project: { ...t.project, resume_agent_sessions: resume } }
          : t,
      ),
    );
    await invoke("set_project_resume_agent_sessions", { id, resume }).catch(() => {});
  }, []);

  const setProjectCollapsed = useCallback(async (id: string, collapsed: boolean) => {
    // Optimistically reflect the disclosure on the tree (so a re-list / reload
    // restores the same open state), then persist. A failure leaves the
    // optimistic flag; the next list corrects it.
    setProjects((prev) =>
      prev.map((t) => (t.project.id === id ? { ...t, project: { ...t.project, collapsed } } : t)),
    );
    await invoke("set_project_collapsed", { id, collapsed }).catch(() => {});
  }, []);

  const setWorkspaceCollapsed = useCallback(async (id: string, collapsed: boolean) => {
    // Optimistically reflect the disclosure on the tree, then persist.
    setProjects((prev) =>
      prev.map((t) => ({
        ...t,
        workspaces: t.workspaces.map((w) => (w.id === id ? { ...w, collapsed } : w)),
      })),
    );
    await invoke("set_workspace_collapsed", { id, collapsed }).catch(() => {});
  }, []);

  return {
    projects,
    loading,
    createProject,
    createWorkspace,
    updateProject,
    deleteProject,
    renameWorkspace,
    setProjectResumeAgentSessions,
    setProjectCollapsed,
    setWorkspaceCollapsed,
  };
}
