import { useCallback, useEffect, useRef, useState } from "react";
import { nyxBridge } from "@/bridge";

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
const WORKSPACES_CHANGED_EVENT = "workspaces://changed" as const;

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
   * The project's canonical KEY — the prefix every Spec of the project is referenced by (`NYX-42`
   * is Spec 42 of the project keyed `NYX`). NOT NULL and UNIQUE backend-side.
   *
   * It is DERIVED from the name at creation and, since the settings pane's Key field, changeable
   * afterwards through `setProjectKey`. A Spec's reference is composed at render time
   * (`key || '-' || number`), never stored — so this field is what every reference on screen reads,
   * and why the write path is the only non-optimistic one on this hook.
   */
  key: string;
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
  /**
   * The container's NATURE (`db::Project.kind`, PRD « Scope Système »): `"user"` for an ordinary
   * human-created project, `"system"` for the SINGLETON technical container of the Système scope.
   *
   * IT IS NEVER `"system"` IN THIS ARRAY. `list_projects` filters `kind = 'user'` backend-side, so
   * every tree `useProjects` holds is a user project — the singleton reaches the UI only through
   * `system_scope_detail` (`components/system/use-system-scope.ts`), deliberately outside the
   * drag-reorderable array. The field is carried here because the WIRE carries it (a renderer that
   * silently dropped a discriminant it receives is how a `system` row eventually slips into a
   * `user` list unnoticed), and it is what a surface asserts against rather than re-deriving the
   * nature from a name or an id.
   *
   * OPTIONAL because a payload built before the `projects.kind` migration (and every test fixture
   * that predates it) omits it; absent reads as `"user"`.
   */
  kind?: "user" | "system";
  /**
   * The project's CHOSEN accent colour, or `null`/absent when it has none. An opaque string the
   * backend stores and never interprets — the renderer resolves it (`<ProjectMark>`), falling
   * back to the neutral tile when it is missing. Optional on the wire so a record built before
   * this column existed (a fixture, an older payload) simply reads as "no accent".
   */
  accent?: string | null;
  /**
   * The project's MARK as an icon name, or `null`/absent. Mutually exclusive with
   * {@link ProjectRecord.image_path} — a backend CHECK enforces it, so at most one is ever set.
   */
  icon?: string | null;
  /**
   * The project's MARK as an image (a file path or URL), or `null`/absent. Mutually exclusive
   * with {@link ProjectRecord.icon}.
   */
  image_path?: string | null;
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

// A `ROOT_WORKSPACE_REMOVE_REASON` used to live here: the sentence explaining why a root
// workspace's Remove entry was rendered DISABLED. Both menus that carried it now render NO
// lifecycle trigger at all on a root (review `01M0A7QGBK43MZB10RF51QR6TS`) — no kebab in
// `terminals/workspace-accordion.tsx`, no per-workspace group in `terminals/base-layout.tsx` —
// so there is no disabled row left to explain, and the constant went with it.

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
   * Delete a SINGLE (non-root) workspace by id. The backend REFUSES exactly two
   * cases — the project's root (delete the whole project instead) and an unknown
   * id; a RUNNING command is NOT one of them, the bridge STOPS it first (review
   * `01KWEX11`, "tout supprimer + tuer les vivants"). The rejection PROPAGATES so
   * the caller can toast the real reason. The delete DESTROYS the workspace's
   * runtime: its command instances cascade-delete, its terminal PTYs are closed,
   * and `usecase::remove_workspace` DELETES its terminal records + agent sessions
   * in the removal transaction — nothing survives detached here (that is
   * `deleteProject`'s contract, not this one). The workspace is dropped from the
   * in-memory tree only on success.
   */
  deleteWorkspace: (id: string) => Promise<void>;
  /**
   * Reorder the top-level project list to match `ids` (the new drag order, FEEDBACK
   * #11). Optimistically reorders the in-memory tree so the sidebar repaints
   * immediately, then persists the new order via the backend `projects_reorder`
   * (which rewrites each project's `order` column). Mirrors the optimistic terminal
   * reorder: a failed persist leaves the optimistic order; the next list /
   * `workspaces://changed` re-list reconciles it.
   */
  reorderProjects: (ids: string[]) => Promise<void>;
  /**
   * Persist a project's `resume_agent_sessions` opt-in (PRD-5 #5). Optimistically
   * reflected on the tree, then persisted via `set_project_resume_agent_sessions`.
   */
  setProjectResumeAgentSessions: (id: string, resume: boolean) => Promise<void>;
  /**
   * Persist a project's KEY — the prefix every Spec of the project is referenced by (`NYX-42`).
   *
   * NOT OPTIMISTIC, unlike every other write on this hook, and that is the decision: this one
   * REFUSES. A malformed key and a key another project already holds both come back as an Err
   * carrying the backend's own sentence, and reflecting the new key on the tree before the answer
   * would repaint every Spec reference in the app under a key that was never granted. So the tree
   * is updated from what the backend RETURNS, after it has agreed.
   *
   * The rejection PROPAGATES so the settings field can print it under the input — which is the
   * only place the user can learn that `NYX` is taken.
   */
  setProjectKey: (id: string, key: string) => Promise<void>;
  /**
   * Persist a project's ACCENT — the name of a palette token (`identity-3`), or
   * `null` to clear it back to the neutral tile. Optimistically reflected on the
   * tree so every `<ProjectMark>` repaints at once, then persisted via
   * `set_project_accent`.
   *
   * CLEARING IS `null`, never `identity-0`: the absence of an accent has one
   * spelling in the database, and `--identity-0` is a render-side neutral the UI
   * never stores.
   */
  setProjectAccent: (id: string, accent: string | null) => Promise<void>;
  /**
   * Persist a project's MARK — its glyph OR its picture, never both. Pass
   * `{ icon }` to set a glyph, `{ image }` to set a picture, or `null` to drop
   * the mark and fall back to the project's initial.
   *
   * The EXCLUSION is not arbitrated here: `set_project_mark` writes both columns,
   * so setting one clears the other, and the optimistic reflection does the same
   * on the tree rather than leaving a stale sibling on screen for one frame.
   */
  setProjectMark: (id: string, mark: { icon: string } | { image: string } | null) => Promise<void>;
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
  const projects = await nyxBridge.invoke<ProjectRecord[]>("list_projects");
  const trees = await Promise.all(
    projects.map(async (project) => {
      const workspaces = await nyxBridge.invoke<WorkspaceRecord[]>("list_workspaces", {
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

  /**
   * Put the tree back on what the database actually holds. The repair path for an optimistic
   * write that was REFUSED: the write is also what would have emitted the corrective
   * `workspaces://changed`, so nothing else is coming. A transient list failure leaves the
   * current tree rather than blanking it — the next event recovers, exactly as the subscription
   * below decides for the same case.
   */
  const syncProjectTrees = useCallback(async () => {
    try {
      setProjects(await loadProjectTrees());
    } catch {
      /* the next event recovers */
    }
  }, []);

  useEffect(() => {
    if (bootstrapped.current) return;
    bootstrapped.current = true;
    void (async () => {
      try {
        const trees = await loadProjectTrees();
        setProjects(trees);
        // THE BOOT SWEEP, and the ONE place it may be called from. Copied project
        // pictures live on disk under ids; a project deleted while nyx was closed —
        // or deleted by an agent, which has no business knowing about the disk —
        // leaves its file behind, and nothing else would ever collect it.
        //
        // It runs HERE and only here, on the success path, AFTER the list resolved:
        // the sweep's argument is "everything alive", so a list that failed or has
        // not landed yet would spell it as "nothing is alive". The main process
        // refuses an empty list for the same reason — two guards, because losing a
        // picture the user chose is not recoverable.
        if (trees.length > 0) {
          void nyxBridge.pruneProjectImages(trees.map((t) => t.project.id));
        }
      } catch {
        // list_projects failed (transient IPC/DB): leave the tree empty; the
        // next mutation re-lists and recovers. NO SWEEP — see above.
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
    void nyxBridge
      .subscribe(WORKSPACES_CHANGED_EVENT, () => {
        if (torndown) return;
        void loadProjectTrees()
          .then((trees) => {
            if (!torndown) setProjects(trees);
          })
          // A transient list failure leaves the current tree; the next event recovers.
          .catch(() => {});
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
  }, []);

  /** Re-list a single project's workspaces and fold them into the tree. */
  const refreshWorkspaces = useCallback(async (projectId: string) => {
    const workspaces = await nyxBridge.invoke<WorkspaceRecord[]>("list_workspaces", {
      projectId,
    });
    setProjects((prev) => prev.map((t) => (t.project.id === projectId ? { ...t, workspaces } : t)));
  }, []);

  const createProject = useCallback(async (name: string, rootPath: string, rootName?: string) => {
    const created = await nyxBridge.invoke<ProjectWithRoot>("create_project", {
      name,
      rootPath,
      rootName: rootName ?? null,
    });
    setProjects((prev) => [...prev, { project: created.project, workspaces: [created.root] }]);
    return created;
  }, []);

  const createWorkspace = useCallback(
    async (projectId: string, name: string, path: string) => {
      const ws = await nyxBridge.invoke<WorkspaceRecord>("create_workspace", {
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
    // Optimistically reflect the new name so the header repaints immediately, then
    // persist. The rejection PROPAGATES (so the caller can toast the real reason and
    // the next list corrects the optimistic name); a `workspaces://changed` re-list
    // or the next load reconciles the tree on failure.
    setProjects((prev) =>
      prev.map((t) => (t.project.id === id ? { ...t, project: { ...t.project, name } } : t)),
    );
    await nyxBridge.invoke("update_project", { id, name });
  }, []);

  const deleteProject = useCallback(async (id: string) => {
    // Await the backend FIRST, then drop the project from the tree only on success.
    // `delete_project` can still fail (Err) — an optimistic-remove-then-swallow would
    // desync the sidebar from the DB, the project vanishing locally while it (and its
    // live processes) survive, and its terminals being detached for nothing. The
    // rejection PROPAGATES so the caller skips the terminal detach and the confirm
    // modal surfaces the message. A RUNNING managed command is not one of those
    // failures any more: the sidecar bridge STOPS every instance of the project before
    // the delete (review `01M0AJ4F6RMS4CHWQ5B6DB47RC`), because the FK cascade erases
    // the `command_instances` rows that are the only handles on those processes.
    // The backend detaches bound terminals (workspace_id → null);
    // `useTerminals` reflects that via its own list/auto-attach passes; here we only
    // own the project/workspace tree. The delete sits behind a confirm dialog with a
    // submitting spinner, so awaiting first costs no perceptible snappiness.
    await nyxBridge.invoke("delete_project", { id });
    setProjects((prev) => prev.filter((t) => t.project.id !== id));
    // The picture goes with the row it belonged to. Leaving it would keep a file on disk
    // for a project that no longer exists until the NEXT boot — the sweep is a net, not a
    // schedule, and this gesture knows exactly which project just went. Best-effort by the
    // bridge's own contract: a locked file must not turn a successful delete into an error.
    await nyxBridge.discardProjectImage(id);
  }, []);

  const deleteWorkspace = useCallback(async (id: string) => {
    // Await the backend FIRST, then drop the workspace from its project's tree only on
    // success. `workspace_delete` still REFUSES (Err) the root workspace and an unknown
    // id — a RUNNING command is NOT a refusal any more, the bridge stops it — so an
    // optimistic-remove-then-swallow would desync the sidebar from the DB (the workspace
    // would vanish locally while it survives). The rejection PROPAGATES so the confirm
    // modal surfaces the real reason.
    // The backend DESTROYS the workspace's runtime rather than detaching it: the bridge stops
    // every running command instance and closes every terminal PTY of the workspace, and
    // `usecase::remove_workspace` deletes its terminal records + agent sessions inside the
    // removal transaction (review `01KWEX11`, "tout supprimer + tuer les vivants") — it does
    // NOT leave them behind as loose terminals the way `delete_project` does. Here we own
    // only the tree; `terminal-manager.tsx`'s `deleteWorkspaceAndDetach` owns what the
    // terminal list does with the now dangling `workspace_id` (this command pushes
    // `workspaces://changed`, not `terminals://changed`, so `useTerminals` does not re-list
    // on it). The delete sits behind a confirm dialog with a submitting spinner, so awaiting
    // first costs no perceptible snappiness.
    await nyxBridge.invoke("workspace_delete", { id });
    setProjects((prev) =>
      prev.map((t) => ({ ...t, workspaces: t.workspaces.filter((w) => w.id !== id) })),
    );
  }, []);

  const renameWorkspace = useCallback(async (id: string, name: string) => {
    // Optimistically reflect the new workspace name across the tree, then persist. The rejection
    // PROPAGATES, exactly like `updateProject`'s just above and for the same reason: the caller is
    // a confirm dialog that SURFACES the real reason inline and toasts a failure. This used to be
    // `.catch(() => {})`, which was inert while `rename_workspace` had no UI caller and became a
    // LIE the day one arrived — the dialog closed on a "Workspace renamed" toast the backend had
    // never granted, leaving the sidebar showing a name the DB refused. A `workspaces://changed`
    // re-list (or the next load) reconciles the optimistic label on failure.
    setProjects((prev) =>
      prev.map((t) => ({
        ...t,
        workspaces: t.workspaces.map((w) => (w.id === id ? { ...w, name } : w)),
      })),
    );
    await nyxBridge.invoke("rename_workspace", { id, name });
  }, []);

  const reorderProjects = useCallback(async (ids: string[]) => {
    // Optimistically reorder the project trees to match the dragged id order so the
    // sidebar repaints immediately, then persist (mirrors the optimistic terminal
    // reorder in `useTerminals`). Trees absent from `ids` (defensive; normally all)
    // are appended in their current order so a stale/foreign id never drops a row.
    setProjects((prev) => {
      const byId = new Map(prev.map((t) => [t.project.id, t]));
      const reordered = ids
        .map((id) => byId.get(id))
        .filter((t): t is ProjectTree => t !== undefined);
      const missing = prev.filter((t) => !ids.includes(t.project.id));
      return [...reordered, ...missing];
    });
    // A failed persist leaves the optimistic order; the next list /
    // `workspaces://changed` re-list reconciles the tree against the DB.
    await nyxBridge.invoke("projects_reorder", { ids }).catch(() => {});
  }, []);

  const setProjectResumeAgentSessions = useCallback(async (id: string, resume: boolean) => {
    // Optimistically reflect the toggle so the dialog/header repaint immediately, then
    // persist. The rejection PROPAGATES so the caller can toast the real reason; the
    // optimistic flag is corrected by the next list/`workspaces://changed` re-list.
    setProjects((prev) =>
      prev.map((t) =>
        t.project.id === id
          ? { ...t, project: { ...t.project, resume_agent_sessions: resume } }
          : t,
      ),
    );
    await nyxBridge.invoke("set_project_resume_agent_sessions", { id, resume });
  }, []);

  const setProjectKey = useCallback(async (id: string, key: string) => {
    // AWAIT FIRST, PAINT SECOND — the inverse of every other write on this hook, because this one
    // has refusals of its own (a shape, and a uniqueness). An optimistic paint would re-key every
    // Spec reference on screen to a prefix the backend may be about to refuse, and the corrective
    // `workspaces://changed` is emitted by the write that FAILED. The route hands back the updated
    // row, so the tree is refreshed from what was actually granted rather than from what was asked.
    const project = await nyxBridge.invoke<ProjectRecord>("set_project_key", { id, key });
    setProjects((prev) => prev.map((t) => (t.project.id === id ? { ...t, project } : t)));
  }, []);

  const setProjectAccent = useCallback(
    async (id: string, accent: string | null) => {
      // Optimistically reflect the colour so every mark of that project — rail tile,
      // home header, corner mark on a cross-project row — repaints at once, then
      // persist. The rejection PROPAGATES so the settings modal can toast the real
      // reason.
      setProjects((prev) =>
        prev.map((t) => (t.project.id === id ? { ...t, project: { ...t.project, accent } } : t)),
      );
      try {
        await nyxBridge.invoke("set_project_accent", { id, accent });
      } catch (e) {
        // RE-READ rather than remember. A refused write leaves the tree showing a colour
        // the database does not have, and nothing else would take it back: the corrective
        // `workspaces://changed` is emitted by the write that just FAILED. Re-listing is
        // also the only answer that stays right when the row moved for another reason
        // (an agent, another surface) between the optimistic paint and the refusal.
        void syncProjectTrees();
        throw e;
      }
    },
    [syncProjectTrees],
  );

  const setProjectMark = useCallback(
    async (id: string, mark: { icon: string } | { image: string } | null) => {
      // ONE SLOT, TWO SPELLINGS — so the optimistic write sets BOTH fields, exactly
      // as `db::set_project_mark` does on the other side. Reflecting only the chosen
      // half would leave the previous glyph next to the new picture for a frame, and
      // that intermediate state is one the backend CHECK says cannot exist.
      const icon = mark && "icon" in mark ? mark.icon : null;
      const image = mark && "image" in mark ? mark.image : null;
      setProjects((prev) =>
        prev.map((t) =>
          t.project.id === id ? { ...t, project: { ...t.project, icon, image_path: image } } : t,
        ),
      );
      // The payload carries exactly ONE of the two keys when setting, so the backend
      // never sees the both-supplied request it would (rightly) refuse. Clearing is
      // spelled `icon: null`, which drops both columns.
      try {
        await nyxBridge.invoke(
          "set_project_mark",
          mark === null ? { id, icon: null } : { id, ...mark },
        );
      } catch (e) {
        // Same repair as `setProjectAccent`: re-read, because the write that would have
        // corrected the tree is the one that just failed. Nothing is discarded on this
        // path — the row still references whatever it referenced.
        void syncProjectTrees();
        throw e;
      }
      // THE COPY DOES NOT OUTLIVE THE REFERENCE — and never predeceases it either.
      // Moving to a glyph, or to no mark at all, dereferences the picture nyx copied
      // into its data dir, so the file goes with it here rather than being left to
      // whichever gesture comes next: the boot sweep spares every file a LIVE project
      // could claim, so it would not collect this copy until the project itself was
      // deleted. Choosing a DIFFERENT picture is the same gesture with something to
      // keep — `adoptProjectImage` deliberately leaves the previous copy in place, and
      // this is the second half that drops it.
      //
      // ORDERED AFTER THE WRITE, in both cases, so a failed persist never leaves a row
      // pointing at a file that is gone. That is the whole reason the cleanup is here
      // and not in the main process: only this side knows the row landed.
      if (image === null) await nyxBridge.discardProjectImage(id);
      else await nyxBridge.discardProjectImage(id, image);
    },
    [syncProjectTrees],
  );

  const setProjectCollapsed = useCallback(async (id: string, collapsed: boolean) => {
    // Optimistically reflect the disclosure on the tree (so a re-list / reload
    // restores the same open state), then persist. A failure leaves the
    // optimistic flag; the next list corrects it.
    setProjects((prev) =>
      prev.map((t) => (t.project.id === id ? { ...t, project: { ...t.project, collapsed } } : t)),
    );
    await nyxBridge.invoke("set_project_collapsed", { id, collapsed }).catch(() => {});
  }, []);

  const setWorkspaceCollapsed = useCallback(async (id: string, collapsed: boolean) => {
    // Optimistically reflect the disclosure on the tree, then persist.
    setProjects((prev) =>
      prev.map((t) => ({
        ...t,
        workspaces: t.workspaces.map((w) => (w.id === id ? { ...w, collapsed } : w)),
      })),
    );
    await nyxBridge.invoke("set_workspace_collapsed", { id, collapsed }).catch(() => {});
  }, []);

  return {
    projects,
    loading,
    createProject,
    createWorkspace,
    updateProject,
    deleteProject,
    deleteWorkspace,
    renameWorkspace,
    reorderProjects,
    setProjectResumeAgentSessions,
    setProjectKey,
    setProjectAccent,
    setProjectMark,
    setProjectCollapsed,
    setWorkspaceCollapsed,
  };
}
