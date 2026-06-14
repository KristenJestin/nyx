import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { ChromeBar } from "@/components/chrome/chrome-bar";
import { resolveDisplayName } from "./auto-label";
import { AppSidebar } from "./app-sidebar";
import { TerminalDeck } from "./terminal-deck";
import { spliceWorkspaceOrder } from "./reorder-utils";
import { useManualAdd } from "./use-manual-add";
import {
  useProjects,
  type ProjectTree,
  type ProjectWithRoot,
  type WorkspaceRecord,
} from "./use-projects";
import { useTerminals, type TerminalRecord } from "./use-terminals";
import { useTerminalShortcuts } from "./use-terminal-shortcuts";

/**
 * The inert end-to-end control seam published on `window.__nyx`. It exposes the
 * multi-terminal control surface the e2e suite (tauri-driver) needs but cannot
 * reach otherwise: xterm paints to a WebGL canvas (no DOM text to read/type),
 * and the sidebar's drag/keyboard intents are awkward to drive over WebDriver.
 *
 * It is INERT in production — nothing in the app reads it; it only mirrors the
 * actions a user performs (create at a cwd, type into a terminal, read its
 * buffer, reorder, close) so the e2e can script the restore scenario and read
 * back state. The buffer read/type delegate to the per-terminal deck seams
 * (`__nyxDeck` / `__nyxDeckInput`), keyed by record id, so they work for hidden
 * panes too.
 */
interface NyxE2eSeam {
  /** Snapshot of the current records (id, cwd, label, status, order). */
  list: () => TerminalRecord[];
  /** Record id of the active (visible) terminal, or null. */
  activeId: () => string | null;
  /** Create a terminal at `cwd` (distinct dirs for the 3-terminal scenario). */
  create: (cwd: string) => Promise<void>;
  /** Make `id` the active terminal. */
  setActive: (id: string) => void;
  /** Close a terminal (marks the record closed → not re-spawned). */
  close: (id: string) => Promise<void>;
  /** Persist a new sidebar order. */
  reorder: (ids: string[]) => Promise<void>;
  /** Read a terminal's xterm buffer by record id (works for hidden panes). */
  readBuffer: (id: string) => string;
  /** Type `data` into a terminal by record id (as keystrokes → PTY runs it). */
  typeInto: (id: string, data: string) => void;

  // --- PRD-2 project / workspace / auto-attach seam (ZE3) -----------------
  // These drive the SAME front hooks + backend commands the sidebar UI uses
  // (`useProjects.createProject`/`createWorkspace`, the backend `terminal_info`
  // /`auto_attach_terminal`). The only thing they bypass is the OS-native folder
  // PICKER (`useManualAdd`), which a WebDriver session cannot operate — the
  // create paths underneath are identical, so this exercises the real flow.

  /** Create a project + its root workspace at `rootPath` (real `create_project`). */
  createProject: (name: string, rootPath: string, rootName?: string) => Promise<ProjectWithRoot>;
  /** Add a (non-root) workspace to a project (real `create_workspace`). */
  createWorkspace: (projectId: string, name: string, path: string) => Promise<WorkspaceRecord>;
  /** The project/workspace tree as the sidebar renders it. */
  listProjects: () => ProjectTree[];
  /** Live PTY id for a record (needed to read `terminal_info`), or null. */
  ptyIdFor: (recordId: string) => number | null;
  /**
   * Live `terminal_info` for a record (its `/proc` cwd on Linux, OSC7 elsewhere)
   * — resolves the record's PTY id then invokes the real backend command.
   */
  terminalInfo: (
    recordId: string,
  ) => Promise<{ cwd: string | null; foreground: string | null } | null>;
  /**
   * Run the REAL auto-attach for a record: read its live cwd (via `terminal_info`)
   * then invoke the backend `auto_attach_terminal`, which applies the hybrid
   * /proc-(Linux)/OSC7 provider + longest-ancestor workspace match. Returns the
   * binding after the pass. The front then reflects the new `workspace_id`.
   */
  autoAttach: (recordId: string) => Promise<{ workspace_id: string | null; changed: boolean }>;
}

/**
 * `<TerminalManager>` — the top-level multi-terminal shell: the thin chrome bar,
 * the left sidebar (navigation), and the terminal deck (N mounted terminals,
 * only the active visible). It owns no state itself — `useTerminals` is the
 * single source of truth — it just wires the sidebar intents, the keyboard
 * shortcuts, and the active-item title into one layout.
 */
export function TerminalManager() {
  const {
    terminals,
    activeId,
    create,
    attach,
    autoAttach,
    detachFromWorkspaces,
    close,
    setActive,
    activeNext,
    activePrev,
    reorder,
  } = useTerminals();

  // Project/workspace tree behind the variant-A sidebar spine.
  const {
    projects,
    createProject,
    createWorkspace,
    updateProject,
    deleteProject,
    setProjectCollapsed,
    setWorkspaceCollapsed,
  } = useProjects();

  // Global new/close/next/prev shortcuts. `close` targets the active terminal.
  const closeActive = useCallback(() => {
    if (activeId !== null) void close(activeId);
  }, [activeId, close]);

  useTerminalShortcuts({
    onNew: () => void create(),
    onClose: closeActive,
    onNext: activeNext,
    onPrev: activePrev,
  });

  // Per-workspace "+" (ZDY): launch a terminal at the workspace path and bind it
  // to that workspace so it lists under the right Terminaux subsection.
  const newTerminalInWorkspace = useCallback(
    async (workspace: WorkspaceRecord) => {
      const row = await create(workspace.path);
      await attach(row.id, workspace.id, "manual");
    },
    [create, attach],
  );

  // A LOOSE (unattached) terminal: created at the default cwd with NO attach, so
  // it lists in the top-level TERMINALS section. It still auto-attaches (moves
  // under a workspace) when its live cwd later resolves to a known workspace.
  const newLooseTerminal = useCallback(() => {
    void create();
  }, [create]);

  // Within-workspace terminal reorder: the subsections hand us the new id
  // sequence for ONE workspace's terminals; `spliceWorkspaceOrder` rebuilds the
  // GLOBAL id sequence (every other terminal keeps its slot) so `reorder` never
  // corrupts cross-workspace order. Within-workspace only (finding F).
  const reorderWorkspaceTerminals = useCallback(
    (workspaceId: string, ids: string[]) => {
      void reorder(spliceWorkspaceOrder(terminals, workspaceId, ids));
    },
    [terminals, reorder],
  );

  // Loose (unattached) terminal reorder (01KV2V4AWT…): the loose TERMINALS
  // section hands us the new id sequence for the `workspace_id == null` group;
  // `spliceWorkspaceOrder(null)` rebuilds the GLOBAL sequence in place so the
  // loose order persists without corrupting any workspace's order.
  const reorderLooseTerminals = useCallback(
    (ids: string[]) => {
      void reorder(spliceWorkspaceOrder(terminals, null, ids));
    },
    [terminals, reorder],
  );

  // Delete a project AND reconcile its terminals: the backend cascades the
  // project's workspaces and SET-NULLs the bound terminals' workspace_id, but
  // `useTerminals` re-lists only at mount — so without mirroring the detach those
  // terminals keep a stale `workspace_id` and the sidebar files them under a
  // now-deleted workspace (rendered by no project) AND excludes them from the
  // loose section, making a live terminal vanish from the UI until relaunch. We
  // capture the project's workspace ids BEFORE the delete, then detach locally.
  const deleteProjectAndDetach = useCallback(
    async (id: string) => {
      const tree = projects.find((p) => p.project.id === id);
      const workspaceIds = tree ? tree.workspaces.map((w) => w.id) : [];
      await deleteProject(id);
      detachFromWorkspaces(workspaceIds);
    },
    [projects, deleteProject, detachFromWorkspaces],
  );

  // Manual add / edit / delete flows (folder-picker-driven).
  const { addProject, addWorkspace, editProject, removeProject, dialog } = useManualAdd({
    createProject,
    createWorkspace,
    updateProject,
    deleteProject: deleteProjectAndDetach,
  });

  // Live record→PTY id map, populated by the deck as each shell spawns/exits.
  // The sidebar reads `terminal_info(ptyId)` per item for the auto label.
  const [ptyIds, setPtyIds] = useState<Map<string, number | null>>(() => new Map());
  const handlePtyId = useCallback((recordId: string, ptyId: number | null) => {
    setPtyIds((prev) => {
      if (prev.get(recordId) === ptyId) return prev; // no-op churn guard
      const next = new Map(prev);
      next.set(recordId, ptyId);
      return next;
    });
  }, []);

  // Latest `ptyIds` / `projects` read through refs so the e2e seam's
  // project/auto-attach methods see current data WITHOUT re-publishing the seam
  // on every PTY-id or project-tree change (the seam identity stays stable).
  const ptyIdsRef = useRef(ptyIds);
  ptyIdsRef.current = ptyIds;
  const projectsRef = useRef(projects);
  projectsRef.current = projects;

  // Production AUTO-ATTACH loop: for each LOOSE, auto-mode terminal that has a
  // live PTY id, read its live cwd (`terminal_info`) and run the real backend
  // `auto_attach_terminal`. When the cwd resolves to a known workspace the
  // terminal binds and the sidebar moves it OUT of the top-level TERMINALS
  // section into that workspace (finding B). Only runs while there is at least
  // one loose terminal AND at least one workspace to match against, polling on a
  // gentle cadence (the backend `terminal_info` is itself debounced to ~1s).
  const hasWorkspaces = projects.some((p) => p.workspaces.length > 0);
  const looseAutoIds = useMemo(
    () =>
      terminals
        .filter((t) => !t.workspace_id && (t.workspace_binding_mode ?? "auto") === "auto")
        .map((t) => t.id)
        .join(","),
    [terminals],
  );
  useEffect(() => {
    // In the e2e build the auto-attach pass is driven DETERMINISTICALLY through
    // the `window.__nyx.autoAttach` seam (so the specs control its timing); a
    // background loop here would race those explicit calls. Disable it under the
    // e2e flag — production keeps the live loop.
    if (import.meta.env.VITE_NYX_E2E === "1") return;
    if (!hasWorkspaces || looseAutoIds === "") return;
    let cancelled = false;
    const ids = looseAutoIds.split(",");
    const pass = async () => {
      for (const id of ids) {
        if (cancelled) return;
        const ptyId = ptyIdsRef.current.get(id);
        if (ptyId == null) continue;
        const info = await invoke<{ cwd: string | null }>("terminal_info", {
          id: ptyId,
        }).catch(() => null);
        if (cancelled) return;
        await autoAttach(id, info?.cwd ?? null);
      }
    };
    void pass();
    const timer = setInterval(() => void pass(), 1500);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [hasWorkspaces, looseAutoIds, autoAttach]);

  // Publish the e2e control seam on window. Refreshed whenever the records or
  // active id change so `list()`/`activeId()` always read the latest.
  //
  // GATED: attached ONLY when the front bundle was built with `VITE_NYX_E2E=1`
  // (the e2e release build — see `bun run build:e2e`). A real production build
  // leaves the flag unset, so this whole block is dead and Vite tree-shakes the
  // seam out — the backend-mutating PRD-2 methods (`createProject` /
  // `createWorkspace` / `terminalInfo` / `autoAttach`) never ship to users.
  useEffect(() => {
    if (import.meta.env.VITE_NYX_E2E !== "1") return;
    const win = window as unknown as { __nyx?: NyxE2eSeam };
    win.__nyx = {
      list: () => terminals,
      activeId: () => activeId,
      create: async (cwd: string) => {
        await create(cwd);
      },
      setActive,
      close,
      reorder,
      readBuffer: (id: string) => {
        const seam = (window as unknown as { __nyxDeck?: Record<string, () => string> }).__nyxDeck;
        return seam?.[id]?.() ?? "";
      },
      typeInto: (id: string, data: string) => {
        const seam = (
          window as unknown as {
            __nyxDeckInput?: Record<string, (data: string) => void>;
          }
        ).__nyxDeckInput;
        seam?.[id]?.(data);
      },

      // --- PRD-2 project / workspace / auto-attach (ZE3) ----------------
      createProject: (name, rootPath, rootName) => createProject(name, rootPath, rootName),
      createWorkspace: (projectId, name, path) => createWorkspace(projectId, name, path),
      listProjects: () => projectsRef.current,
      ptyIdFor: (recordId) => ptyIdsRef.current.get(recordId) ?? null,
      terminalInfo: async (recordId) => {
        const ptyId = ptyIdsRef.current.get(recordId);
        if (ptyId == null) return null;
        return invoke<{ cwd: string | null; foreground: string | null }>("terminal_info", {
          id: ptyId,
        });
      },
      autoAttach: async (recordId) => {
        // Read the terminal's live cwd from the backend (`/proc` on Linux),
        // exactly as a real auto-attach pass would, then run the real resolver.
        const ptyId = ptyIdsRef.current.get(recordId);
        let cwd: string | null = null;
        if (ptyId != null) {
          const info = await invoke<{ cwd: string | null }>("terminal_info", {
            id: ptyId,
          }).catch(() => null);
          cwd = info?.cwd ?? null;
        }
        const res = await invoke<{
          workspace_id: string | null;
          changed: boolean;
        }>("auto_attach_terminal", { terminalId: recordId, cwd });
        // Reflect the (auto) binding locally so `list()` shows the new
        // workspace_id, mirroring what a UI-driven auto-attach would do. Gate on
        // `changed` (like the production `useTerminals.autoAttach`): when the
        // backend left the binding alone (no match, or a MANUAL pin it must not
        // move) `changed` is false even though `workspace_id` is the current one
        // — re-attaching with mode "auto" there would silently un-pin a manual
        // terminal.
        if (res.changed && res.workspace_id) {
          await attach(recordId, res.workspace_id, "auto");
        }
        return res;
      },
    };
    return () => {
      delete (window as unknown as { __nyx?: NyxE2eSeam }).__nyx;
    };
  }, [
    terminals,
    activeId,
    create,
    setActive,
    close,
    reorder,
    createProject,
    createWorkspace,
    attach,
  ]);

  // Discreet active-item title for the chrome bar (manual label wins; auto/cwd
  // fall back). The chrome title uses the record-only resolution — the live auto
  // label is rendered per-item in the sidebar.
  const activeIndex = terminals.findIndex((t) => t.id === activeId);
  const title =
    activeIndex === -1 ? undefined : resolveDisplayName(terminals[activeIndex], activeIndex, null);

  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden bg-background">
      <ChromeBar title={title} />
      <div className="flex min-h-0 flex-1">
        <AppSidebar
          projects={projects}
          terminals={terminals}
          activeId={activeId}
          ptyIds={ptyIds}
          onSelect={setActive}
          onClose={(id) => void close(id)}
          onNewTerminal={(ws) => void newTerminalInWorkspace(ws)}
          onNewLooseTerminal={newLooseTerminal}
          onAddProject={() => void addProject()}
          onAddWorkspace={(tree) => void addWorkspace(tree)}
          onEditProject={(tree) => editProject(tree)}
          onDeleteProject={(tree) => removeProject(tree)}
          onReorderTerminals={reorderWorkspaceTerminals}
          onReorderLooseTerminals={reorderLooseTerminals}
          onSetProjectCollapsed={(id, collapsed) => void setProjectCollapsed(id, collapsed)}
          onSetWorkspaceCollapsed={(id, collapsed) => void setWorkspaceCollapsed(id, collapsed)}
        />
        <div className="min-w-0 flex-1">
          <TerminalDeck terminals={terminals} activeId={activeId} onPtyId={handlePtyId} />
        </div>
      </div>
      {dialog}
    </div>
  );
}
