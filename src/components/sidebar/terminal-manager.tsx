import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { ChromeBar } from "@/components/chrome/chrome-bar";
import { CommandView } from "@/components/command/command-view";
import { ProjectCommandsDialog } from "@/components/command/project-commands-dialog";
import { SettingsDialog, type SettingsDialogHandle } from "./settings-dialog";
import { useCommandInstances } from "@/components/command/use-command-instances";
import type { CommandStatePayload } from "@/components/command/use-command-state";
import type { InstanceWithTemplate, ManagedCommand } from "@/components/command/use-commands";
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
import { useTerminals, type ExecState, type TerminalRecord } from "./use-terminals";
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

  // --- PRD-3 managed-command seam -----------------------------------------
  // These drive the SAME backend commands the command UI uses (`command_create`,
  // `command_instance_list`, `command_start`/`stop`/`relaunch`, `command_output`)
  // plus a live read of the run-state dot. xterm paints command output to a WebGL
  // canvas too, so the e2e reads the instance's output + 4-state run status through
  // this seam rather than the DOM. INERT in production (nothing reads it).

  /** Create a project command TEMPLATE (materializes one instance per workspace). */
  createCommand: (
    projectId: string,
    name: string,
    command: string,
    subfolder?: string | null,
  ) => Promise<ManagedCommand>;
  /** List a workspace's command instances (id + live last_state + joined name). */
  listCommandInstances: (workspaceId: string) => Promise<InstanceWithTemplate[]>;
  /** Start an instance; resolves to the run state after the call (e.g. "running"). */
  commandStart: (instanceId: string) => Promise<string>;
  /** Stop an instance (process-tree kill → idle); resolves to the state after. */
  commandStop: (instanceId: string) => Promise<string>;
  /** Relaunch an instance (stop-then-start if running, else direct start). */
  commandRelaunch: (instanceId: string) => Promise<string>;
  /**
   * The instance's LATEST observed run state for the dot (idle|running|success|
   * error), tracked from the `command://state` event stream — the exact signal the
   * status dot renders. Falls back to `idle` for an unseen instance.
   */
  commandState: (instanceId: string) => ExecState;
  /** The instance's output history (live buffer while running, else persisted). */
  commandOutput: (instanceId: string) => Promise<string>;
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
    markRead,
  } = useTerminals();

  // Project/workspace tree behind the variant-A sidebar spine.
  const {
    projects,
    createProject,
    createWorkspace,
    updateProject,
    setProjectResumeAgentSessions,
    deleteProject,
    setProjectCollapsed,
    setWorkspaceCollapsed,
  } = useProjects();

  // PRD-3 command INSTANCES per workspace (sidebar COMMANDS band + main-pane view).
  const {
    instances,
    commandsByWorkspace,
    refresh: refreshCommands,
  } = useCommandInstances(projects);

  // The global Settings modal (gear icon in the sidebar head). The dialog stays
  // mounted (Motion animates its exit); we pull a fresh provider list ON THE
  // OPEN EVENT via its imperative handle — not from an effect watching `open`.
  const [settingsOpen, setSettingsOpen] = useState(false);
  const settingsDialogRef = useRef<SettingsDialogHandle>(null);
  const openSettings = useCallback(() => {
    setSettingsOpen(true);
    settingsDialogRef.current?.reload();
  }, []);

  // The "Manage commands" modal: which project's commands are being managed.
  const [manageProject, setManageProject] = useState<ProjectTree | null>(null);
  // The selected command instance (its `<CommandView>` mounts in the main pane).
  const [activeCommandId, setActiveCommandId] = useState<string | null>(null);
  const activeCommand = useMemo(
    () => instances.find((i) => i.id === activeCommandId) ?? null,
    [instances, activeCommandId],
  );

  // Selecting a command shows its view; selecting a TERMINAL clears the active
  // command (one main-pane surface at a time — terminal deck OR command view).
  //
  // ACKNOWLEDGE-ON-SELECT: a finished one-shot's success/error result is an "unseen
  // result"; OPENING the command means the user saw it, so we clear ONLY its `unread`
  // flag (PRD-4 v4) — the FACTUAL state + exit code are preserved (the backend emits
  // `command://ack`, never an idle `command://state`, so the dot keeps the factual
  // outcome and the MCP still sees the error). We invoke `command_acknowledge` only
  // for a still-UNREAD terminal result — never for a running/idle command, and never
  // for an already-acknowledged one (gating here avoids a useless round-trip; the
  // backend also no-ops).
  const selectCommand = useCallback(
    (id: string) => {
      setActiveCommandId(id);
      const inst = instances.find((i) => i.id === id);
      if (inst && inst.unread && (inst.state === "success" || inst.state === "error")) {
        void invoke("command_acknowledge", { instanceId: id }).catch(() => {});
      }
    },
    [instances],
  );
  // Selecting a terminal VIEWS it: switch the deck to it AND mark its settled
  // exec-state read (PRD-2.1) — viewing a success/error terminal clears its unread
  // badge (and persists the clear). `running` is unaffected (it has no unread bit);
  // `markRead` no-ops when the terminal is already read.
  const selectTerminal = useCallback(
    (id: string) => {
      setActiveCommandId(null);
      setActive(id);
      markRead(id);
    },
    [setActive, markRead],
  );

  // ACTIVE-SETTLE (PRD-2.1): if a `success`/`error` exec-state arrives for the
  // terminal that is CURRENTLY being VIEWED (the active terminal, with no command
  // view covering the deck), mark it read at once — the user is already looking at
  // it, so it must never accumulate an unread badge. `useTerminals` folds the event
  // onto the record (setting `exec_state_unread`), and this effect reacts to that
  // record turning unread-while-viewed and calls the mark-read path. A terminal
  // viewed AFTER it settled is handled by `selectTerminal`'s `markRead`; this covers
  // the event-arrives-while-already-active case. Gated on `activeCommandId === null`
  // because an open command view HIDES the deck (the terminal is not truly viewed).
  const viewedTerminal = useMemo(
    () =>
      activeCommandId === null && activeId !== null
        ? (terminals.find((t) => t.id === activeId) ?? null)
        : null,
    [activeCommandId, activeId, terminals],
  );
  useEffect(() => {
    if (viewedTerminal?.exec_state_unread) {
      markRead(viewedTerminal.id);
    }
  }, [viewedTerminal, markRead]);

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
      // Detach ONLY after a successful delete: `deleteProject` now rejects when the
      // backend refuses (e.g. a running command), so the throw skips the detach and
      // propagates to the confirm modal — never detaching terminals of a project
      // that still exists.
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
    setProjectResumeAgentSessions,
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
    // Surface the record↔PTY link to the backend so the MCP terminal tools
    // (send_to_terminal / close / list_terminals) can resolve a record id to its
    // live PTY. The front still owns the PTY lifecycle (the `<Terminal>` spawned it);
    // this only publishes the join the front already maintains. Registering a live
    // id ALSO lets the backend inject any command the MCP `create_terminal` tool
    // parked for this record at opening. Fire-and-forget: a failed register just
    // means an agent cannot reach this particular terminal until the next spawn.
    void invoke("register_terminal_pty", { recordId, ptyId }).catch(() => {});
  }, []);

  // Latest `ptyIds` / `projects` read through refs so the e2e seam's
  // project/auto-attach methods see current data WITHOUT re-publishing the seam
  // on every PTY-id or project-tree change (the seam identity stays stable).
  const ptyIdsRef = useRef(ptyIds);
  ptyIdsRef.current = ptyIds;
  const projectsRef = useRef(projects);
  projectsRef.current = projects;

  // E2E ONLY: a live map of each command instance's latest run state, fed by the
  // backend `command://state` stream. The command-seam's `commandState(id)` reads
  // this so the e2e can observe the dot's 4 states (idle/running/success/error)
  // without the WebGL-painted output. Gated behind the e2e flag so the listener is
  // dead code (tree-shaken) in a real production build — it never ships to users.
  const commandStatesRef = useRef<Map<string, ExecState> | null>(null);
  commandStatesRef.current ??= new Map();
  const commandStates = commandStatesRef.current;
  useEffect(() => {
    if (import.meta.env.VITE_NYX_E2E !== "1") return;
    let torndown = false;
    let unlisten: (() => void) | undefined;
    void listen<CommandStatePayload>("command://state", (event) => {
      if (torndown) return;
      const { instanceId, state } = event.payload;
      commandStates.set(instanceId, state);
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

      // --- PRD-3 managed-command seam -----------------------------------
      // Each method is a thin pass-through to the SAME backend command the
      // command UI invokes; `commandState` reads the live `command://state` map
      // above. No production code reads these (inert seam).
      createCommand: (projectId, name, command, subfolder) =>
        invoke<ManagedCommand>("command_create", {
          projectId,
          name,
          command,
          subfolder: subfolder && subfolder.trim() ? subfolder.trim() : null,
          restartOnStartup: false,
        }),
      listCommandInstances: (workspaceId) =>
        invoke<InstanceWithTemplate[]>("command_instance_list", { workspaceId }),
      commandStart: (instanceId) => invoke<string>("command_start", { instanceId }),
      commandStop: (instanceId) => invoke<string>("command_stop", { instanceId }),
      commandRelaunch: (instanceId) => invoke<string>("command_relaunch", { instanceId }),
      commandState: (instanceId) => commandStates.get(instanceId) ?? "idle",
      commandOutput: (instanceId) => invoke<string>("command_output", { instanceId }),
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
          activeId={activeCommandId === null ? activeId : null}
          ptyIds={ptyIds}
          commandsByWorkspace={commandsByWorkspace}
          activeCommandId={activeCommandId}
          onSelectCommand={selectCommand}
          onSelect={selectTerminal}
          onClose={(id) => void close(id)}
          onNewTerminal={(ws) => void newTerminalInWorkspace(ws)}
          onNewLooseTerminal={newLooseTerminal}
          onAddProject={() => void addProject()}
          onAddWorkspace={(tree) => void addWorkspace(tree)}
          onEditProject={(tree) => editProject(tree)}
          onDeleteProject={(tree) => removeProject(tree)}
          onManageCommands={(tree) => setManageProject(tree)}
          onReorderTerminals={reorderWorkspaceTerminals}
          onReorderLooseTerminals={reorderLooseTerminals}
          onSetProjectCollapsed={(id, collapsed) => void setProjectCollapsed(id, collapsed)}
          onSetWorkspaceCollapsed={(id, collapsed) => void setWorkspaceCollapsed(id, collapsed)}
          onOpenSettings={openSettings}
        />
        <div className="min-w-0 flex-1">
          {/* Main pane: the selected COMMAND view (panel + dot + 3 buttons, no
              stdin) when a command is active, else the terminal deck. The deck
              stays mounted underneath so its terminals keep their PTYs alive. */}
          <div className={activeCommand ? "hidden" : "h-full"}>
            <TerminalDeck terminals={terminals} activeId={activeId} onPtyId={handlePtyId} />
          </div>
          {activeCommand && (
            <CommandView
              key={activeCommand.id}
              instanceId={activeCommand.id}
              name={activeCommand.name}
              initialState={activeCommand.state}
              command={activeCommand.command}
              cwd={activeCommand.cwd}
              sourceScriptName={activeCommand.sourceScriptName}
              sourcePackageJsonPath={activeCommand.sourcePackageJsonPath}
            />
          )}
        </div>
      </div>
      {dialog}
      {/* Global Settings modal (gear icon in the sidebar head → Integrations). */}
      <SettingsDialog
        ref={settingsDialogRef}
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
      />
      {/* PRD-3 "Manage commands" modal. Scans the project's ROOT workspace for
          package.json imports (and uses its path to relativize the subfolder
          picker). On close, re-list the instances so newly created / imported /
          deleted commands appear (or disappear) in the sidebar band. */}
      {(() => {
        // The workspace the modal is scoped to: the ROOT, else the first. Both its
        // id (import discovery) and path (subfolder picker relativization) come
        // from the SAME workspace so the picker resolves against what runs.
        const manageWorkspace =
          manageProject?.workspaces.find((w) => w.is_root) ?? manageProject?.workspaces[0] ?? null;
        return (
          <ProjectCommandsDialog
            open={manageProject !== null}
            projectId={manageProject?.project.id ?? null}
            projectName={manageProject?.project.name ?? ""}
            importWorkspaceId={manageWorkspace?.id ?? null}
            workspacePath={manageWorkspace?.path ?? null}
            onClose={() => {
              setManageProject(null);
              void refreshCommands();
            }}
          />
        );
      })()}
    </div>
  );
}
