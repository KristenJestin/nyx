import { useCallback, useEffect, useRef, useState } from "react";
import { errorText, nyxBridge } from "@/bridge";

import type { ProjectRecord, WorkspaceRecord } from "@/components/sidebar/use-projects";

/**
 * The tree-mutation broadcast (`bridge::WORKSPACES_CHANGED_EVENT`). The Système singleton is
 * bootstrapped by `Db::open` and is not renamable/deletable, so it changes far less than a user
 * project — but the FIRST read can legitimately land before the bootstrap on a brand-new database,
 * and a repaired primary workspace (the phase-1 `ensure_system_scope` repair) shows up here too.
 * Re-pulling on the same topic `useProjects` listens to keeps the two reads in step with no second
 * mechanism.
 */
const WORKSPACES_CHANGED_EVENT = "workspaces://changed" as const;

/**
 * The payload of the `system_scope_detail` db-command: the singleton's technical project, its ONE
 * primary workspace and the ABSOLUTE home both are pinned at.
 *
 * `project.kind` is `"system"` here by construction — the read is the singleton's own door.
 */
export interface SystemScopeDetail {
  readonly project: ProjectRecord;
  readonly workspace: WorkspaceRecord;
  /** The absolute platform home: the workspace's `root_path`, and the cwd of every member. */
  readonly home: string;
}

export interface UseSystemScope {
  /** The scope, or `null` while loading / when the backend has none (pre-bootstrap DB). */
  scope: SystemScopeDetail | null;
  /** `true` until the first read resolves (either way). */
  loading: boolean;
  /** The backend's own refusal wording when the read FAILED, else `null`. */
  error: string | null;
  /** Re-run the read on demand (the error state's Retry). */
  refresh: () => Promise<void>;
}

/**
 * `useSystemScope` — the renderer's ONE door onto the Système singleton.
 *
 * ## Why it is not `useProjects`
 *
 * `list_projects` / `list_workspaces` are USER-ONLY by backend contract (`db::list_projects`
 * filters `kind = 'user'`), and that is deliberate rather than incidental: the rail's project array
 * is DRAG-REORDERABLE and persisted through `projects_reorder`, so a technical container inside it
 * would be a row the user can drag, rename by reflex, and whose order the backend would be asked to
 * persist for a project that has no order. The singleton therefore arrives through its own read —
 * `system_scope_detail`, IPC-only, never an MCP tool — and lands OUTSIDE the reorderable array.
 *
 * ## What it deliberately does NOT return
 *
 * The RUNTIME. `system_scope_detail` answers `{ project, workspace, home }` and stops there; the
 * live terminals + commands come from `workspace_runtime_snapshot` over `workspace.id`, through the
 * very `useProjectRuntime` every project page uses. Composing the two HERE would have meant a second
 * runtime read model for one workspace — a shape nothing else in the app consumes, kept live by a
 * second subscription, drifting from the one the rest of the tree reads. The two reads compose at
 * the PAGE instead (`<SystemScopePage>`): identity here, runtime there.
 *
 * A `null` scope is a legitimate answer, not an error: a database that never went through
 * `Db::open` has no singleton, and the read reports the absence instead of bootstrapping behind the
 * caller's back. The page renders its own "unavailable" state for it.
 */
export function useSystemScope(): UseSystemScope {
  const [scope, setScope] = useState<SystemScopeDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Guards a late resolve after unmount (StrictMode's double-mount included) from writing state.
  const aliveRef = useRef(true);
  useEffect(() => {
    aliveRef.current = true;
    return () => {
      aliveRef.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    try {
      const detail = await nyxBridge.invoke<SystemScopeDetail | null>("system_scope_detail");
      if (!aliveRef.current) return;
      setScope(detail ?? null);
      setError(null);
    } catch (e) {
      if (!aliveRef.current) return;
      // The backend's REAL reason, never a generic sentence: a refused/failed read is the one thing
      // the user can act on (a boot that refused an incoherent scope says exactly that).
      setError(errorText(e, "The System scope could not be read."));
    } finally {
      if (aliveRef.current) setLoading(false);
    }
  }, []);

  const refresh = useCallback(async () => {
    setLoading(true);
    await load();
  }, [load]);

  useEffect(() => {
    void load();
  }, [load]);

  // Live: re-read on the same tree topic the project list follows. A transient failure leaves the
  // last good scope on screen; the next event (or the Retry) recovers.
  useEffect(() => {
    let torndown = false;
    let unlisten: (() => void) | undefined;
    void nyxBridge
      .subscribe(WORKSPACES_CHANGED_EVENT, () => {
        if (torndown) return;
        void load();
      })
      .then((un) => {
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
  }, [load]);

  return { scope, loading, error, refresh };
}
