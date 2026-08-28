import { useEffect, useMemo, useRef, useState } from "react";
import { getRouteApi } from "@tanstack/react-router";
import { FolderPlusIcon, SettingsIcon, SquareTerminalIcon } from "lucide-react";

import { useProjects, type WorkspaceRecord } from "@/components/sidebar/use-projects";
import { useManualAdd } from "@/components/sidebar/use-manual-add";
import { SidebarShell } from "@/components/ui/sidebar-shell";
import { CommandGroupsProvider } from "@/components/command/command-groups-context";
import { useCommandGroups } from "@/components/command/use-command-groups";
import {
  ProjectSettingsDialog,
  type ProjectSettingsSection,
} from "@/components/sidebar/project-settings-dialog";
import { TerminalStatsProvider } from "@/components/sidebar/use-terminal-stats";
import { TerminalRenameProvider } from "@/components/sidebar/use-terminal-rename";
import { PagePlaceholder } from "./page-placeholder";
import { useActiveProject } from "./active-project";
import { useProjectRuntime } from "./use-project-runtime";
import { RuntimeSelectionProvider, useRuntimeSelection } from "./runtime-selection";
import { ConsumedNotificationsProvider } from "./use-consumed-notifications";
import { RuntimePaneHost } from "./runtime-pane-host";
import { WorkspaceAccordion } from "./workspace-accordion";
import { useTerminalActions, type PaneShortcutHandlers } from "./use-terminal-actions";
import { groupMemberIds } from "@/components/panes/pane-groups";
import { usePaneLayout, type PaneLayoutController } from "./use-pane-layout";
import { TerminalCloseProvider } from "./use-terminal-close";
import { TerminalGroupsProvider } from "./use-terminal-groups";
import { TerminalReorderProvider } from "./use-terminal-reorder";
import { useLazyResume } from "./use-lazy-resume";
import { useIdleUnload } from "./use-idle-unload";
import { loadLastTerminalId, saveLastTerminalId } from "./ui-persistence";
import type {
  WorkspaceRuntimeService,
  WorkspaceRuntimeTerminal,
} from "@/components/runtime/use-workspace-runtime";

/** The `/` route API, for reading the `?terminal=<id>` deep-link search param. */
const terminalsRouteApi = getRouteApi("/");

/**
 * `<TerminalsPage>` — the index route (`/`). One ENTERS a PROJECT (from the left rail); this
 * page lists the entered project's WORKSPACES as an accordion and, per workspace, its live
 * runtime tree (terminals + commands; agents on their terminals — no 3rd category) plus the
 * main area, which is a TILED HOST: several terminals are alive AND VISIBLE at once, one per
 * pane of a split tree, and exactly ONE of them holds the focus (the one the sidebar's
 * selection follows, in both directions).
 *
 * The workspace stays the runtime PIVOT in the DB (PDR-0008) but is no longer a screen — it
 * appears only as a grouping inside the entered project. The runtime is fed LIVE by the
 * PRESERVED backend read model (`workspace_runtime_snapshot`, re-pulled on `*://changed`
 * pushes with NO polling — see `use-project-runtime`).
 *
 * THE PANE HOST IS NOT THE DECK ANY MORE. The mono-visible `<TerminalDeck>` — one mounted
 * xterm shown, every other one hidden — was REPLACED by the tree host this PRD introduced:
 * the page mounts `<RuntimePaneHost>` (below), which owns the `usePaneLayout` controller and
 * renders `<PaneTreeHost>`, the host that lays every leaf of the pane tree out and mounts a
 * live xterm in each. Each pane still reuses the SAME module-level terminal store (no second
 * terminal per record); what changed is how many of them are on screen. `<TerminalDeck>`
 * survives only as an unmounted legacy shell under `terminal-manager.tsx`. No business logic
 * in the renderer.
 */
export function TerminalsPage() {
  const { activeProjectId } = useActiveProject();
  const {
    projects,
    createProject,
    createWorkspace,
    setProjectResumeAgentSessions,
    updateProject,
    deleteWorkspace,
    renameWorkspace,
  } = useProjects();
  // The optional `?terminal=<id>` / `?command=<id>` deep-links (the Runtime tab opens a precise
  // terminal or managed command here).
  const { terminal: deepLinkTerminalId, command: deepLinkCommandId } =
    terminalsRouteApi.useSearch();

  // "Add workspace" for the entered project — the SAME folder-picker → name-dialog →
  // `create_workspace` flow the old sidebar exposed, reused verbatim via `useManualAdd` (no new
  // backend, no duplicated picker/dialog wiring), mirroring how `<BaseLayout>` reuses it for
  // "Add project". PDR-0025's shell only surfaced project-add (the rail); this re-exposes the
  // existing add-workspace capability where it belongs — the Terminals sidebar (its header, next
  // to the entered project). `dialog` is the mounted modal set, rendered once below.
  // The SAME hook also owns the per-workspace lifecycle flows the accordion headers now expose —
  // rename (`rename_workspace`) and remove (`workspace_delete` behind the existing destructive
  // confirm). Before this, both were reachable ONLY from the project's rail menu, so a worktree
  // offered no way to act on itself (review `01M04WQEQDDPH7NQEK9MPYCXKS`). No new backend command
  // and no second confirm dialog: the capability existed, it just had no entry point here.
  const {
    addWorkspace,
    editWorkspace,
    removeWorkspace,
    dialog: manualAddDialog,
  } = useManualAdd({
    createProject,
    createWorkspace,
    deleteWorkspace,
    renameWorkspace,
  });

  /**
   * THE PROJECT-SETTINGS MODAL — ONE mount, TWO entry points (finding 01M132YSBKJ5J22VG60P5GN7W0).
   *
   * The header's gear opens it on GLOBAL; a workspace's COMMANDS gear opens it on COMMANDS, scoped
   * to that workspace for the package.json import and the subfolder picker. That second entry used
   * to open `<ProjectCommandsDialog>` — a 44rem standalone modal whose body was the very same
   * `<CommandsSection>` this one shows at 60rem. One body, two useful widths, two chromes to
   * maintain: the same duplication as the two lists inside it, one level up. The standalone modal
   * is deleted; the gear opens the settings on the right section, which they already knew how to do
   * (`initialSection`, which `terminal-manager.tsx` was already using).
   *
   * `null` = closed. The value carries the section AND the workspace, so the two entries differ by
   * data rather than by a second component.
   */
  const [settingsTarget, setSettingsTarget] = useState<{
    section: ProjectSettingsSection;
    workspace: { id: string; path: string } | null;
  } | null>(null);

  const activeTree = useMemo(
    () => projects.find((p) => p.project.id === activeProjectId) ?? null,
    [projects, activeProjectId],
  );
  const workspaces = activeTree?.workspaces ?? [];
  const workspaceIds = useMemo(() => workspaces.map((w) => w.id), [workspaces]);

  const { runtimeByWorkspace } = useProjectRuntime(workspaceIds);

  // LAZY agent-session resume (v2.5.5): consume each workspace's `workspace_restore_plan` and hand
  // the sidecar the resumable `claude --resume <id>` lines to inject once at each terminal's PTY
  // open. Honours the project resume toggle (a toggle-OFF workspace yields no resumable entry).
  useLazyResume(workspaceIds);

  // The FLAT list of every workspace's terminals — the deck spans the whole project so a
  // selection switch across workspaces never remounts a live terminal. Kept in workspace
  // order for stable identity (the deck itself re-sorts panes by id internally).
  const allTerminals = useMemo<WorkspaceRuntimeTerminal[]>(() => {
    const out: WorkspaceRuntimeTerminal[] = [];
    for (const id of workspaceIds) {
      const snapshot = runtimeByWorkspace.get(id);
      if (snapshot) out.push(...snapshot.terminals);
    }
    return out;
  }, [workspaceIds, runtimeByWorkspace]);

  // The matching FLAT list of every workspace's managed-command services, so the pane host can
  // read a selected service's live run-state for its idle empty state.
  const allServices = useMemo<WorkspaceRuntimeService[]>(() => {
    const out: WorkspaceRuntimeService[] = [];
    for (const id of workspaceIds) {
      const snapshot = runtimeByWorkspace.get(id);
      if (snapshot) out.push(...snapshot.services);
    }
    return out;
  }, [workspaceIds, runtimeByWorkspace]);

  if (activeTree == null) {
    return (
      <PagePlaceholder
        icon={<SquareTerminalIcon className="size-8" />}
        title="Terminals"
        description="Enter a project from the rail to see its workspaces and runtime."
      />
    );
  }

  return (
    // The ONE `terminal://stats` subscription for the whole page (FEEDBACK #28): mounted OUTSIDE
    // the project-keyed selection provider so it persists across project switches (no churn), and
    // as an ancestor of every terminal row so each reads its live CPU%/RAM (shown on hover only —
    // non-inline). Cheap when nothing streams (an empty map).
    <TerminalStatsProvider>
      {/* Keyed by project so ENTERING a different project (or returning to Terminals) gives a
          fresh selection state — the auto-select effect then RESTORES that project's last active
          terminal (persisted) rather than the previous project's selection leaking across the
          switch. */}
      <RuntimeSelectionProvider key={activeTree.project.id}>
        <ConsumedNotificationsProvider>
          {/* (v2.5.8: the post-crash RestorePlanProvider / "Interrupted — Resume/Ignore" registry
              was reverted. A crash is treated as a clean shutdown → sessions resume SILENTLY via
              `use-lazy-resume`, gated by the project resume toggle. No restore registry needed.) */}
          <TerminalsBody
            projectId={activeTree.project.id}
            projectName={activeTree.project.name}
            workspaces={workspaces}
            runtimeByWorkspace={runtimeByWorkspace}
            allTerminals={allTerminals}
            allServices={allServices}
            deepLinkTerminalId={deepLinkTerminalId ?? null}
            deepLinkCommandId={deepLinkCommandId ?? null}
            onAddWorkspace={() => void addWorkspace(activeTree)}
            onRenameWorkspace={editWorkspace}
            onRemoveWorkspace={removeWorkspace}
            onOpenProjectSettings={() => setSettingsTarget({ section: "global", workspace: null })}
            onOpenCommands={(workspace) => setSettingsTarget({ section: "commands", workspace })}
          />
        </ConsumedNotificationsProvider>
      </RuntimeSelectionProvider>
      {/* The add-workspace modal set (folder picker → name dialog). Mounted once, portalled. */}
      {manualAddDialog}
      {/* THE project-settings modal — mounted ONCE, opened by the header's VISIBLE gear (Global,
          the `resume_agent_sessions` toggle) AND by a workspace's COMMANDS gear (Commands, scoped
          to that workspace). Rename stays hidden (`showRename={false}`): rename/delete belong to
          the PRD-B kebab CRUD, so `onRename` is wired (required by the prop) but unreachable.
          `activeTree` is non-null in this branch (the early return above guards it), and it is
          derived live from `projects`, so the toggle reflects the optimistic tree update
          immediately.

          KEYED ON THE SECTION because the dialog seeds its active section ONCE, on mount: without
          the key, opening it on Global and then pressing a COMMANDS gear would land on Global. */}
      <ProjectSettingsDialog
        key={settingsTarget?.section ?? "closed"}
        open={settingsTarget != null}
        projectId={activeTree.project.id}
        projectName={activeTree.project.name}
        resumeAgentSessions={activeTree.project.resume_agent_sessions ?? false}
        initialSection={settingsTarget?.section ?? "global"}
        importWorkspaceId={settingsTarget?.workspace?.id ?? null}
        workspacePath={settingsTarget?.workspace?.path ?? null}
        showRename={false}
        onRename={(name) => updateProject(activeTree.project.id, name)}
        onResumeChange={(resume) => setProjectResumeAgentSessions(activeTree.project.id, resume)}
        onClose={() => setSettingsTarget(null)}
      />
    </TerminalStatsProvider>
  );
}

/**
 * The composed page layout once inside the selection provider: the workspace accordion column
 * over the shared pane host. Split out so the auto-select effect can read the selection
 * context (it lives under the provider).
 */
function TerminalsBody({
  projectId,
  projectName,
  workspaces,
  runtimeByWorkspace,
  allTerminals,
  allServices,
  deepLinkTerminalId,
  deepLinkCommandId,
  onAddWorkspace,
  onRenameWorkspace,
  onRemoveWorkspace,
  onOpenProjectSettings,
  onOpenCommands,
}: {
  projectId: string;
  projectName: string;
  workspaces: ReturnType<typeof useProjects>["projects"][number]["workspaces"];
  runtimeByWorkspace: ReturnType<typeof useProjectRuntime>["runtimeByWorkspace"];
  allTerminals: readonly WorkspaceRuntimeTerminal[];
  allServices: readonly WorkspaceRuntimeService[];
  /** The `?terminal=<id>` deep-link target — select THIS terminal on entry (else null). */
  deepLinkTerminalId: string | null;
  /** The `?command=<id>` deep-link target — select THIS managed command on entry (else null). */
  deepLinkCommandId: string | null;
  /** Open the add-workspace flow (folder picker → name dialog) for the entered project. */
  onAddWorkspace: () => void;
  /** Open the RENAME dialog for ONE workspace of this project (`rename_workspace`). */
  onRenameWorkspace: (workspace: WorkspaceRecord) => void;
  /** Open the destructive-confirm flow for ONE workspace of this project (`workspace_delete`). */
  onRemoveWorkspace: (workspace: WorkspaceRecord) => void;
  /** Open the project-settings modal (Global pane → resume toggle) for the entered project. */
  onOpenProjectSettings: () => void;
  /**
   * Open THE SAME modal on its Commands section, scoped to a workspace for the package.json import
   * and the subfolder picker — what a COMMANDS-section gear does now that the standalone commands
   * modal is gone (finding 01M132YSBKJ5J22VG60P5GN7W0).
   */
  onOpenCommands: (workspace: { id: string; path: string }) => void;
}) {
  const { selected, select } = useRuntimeSelection();

  // Restore the old shell's terminal management, recabled to the page selection: Ctrl+T (new
  // terminal in the SELECTED terminal's workspace, else loose), Ctrl+W (close selected), the
  // Ctrl+Tab / Ctrl+PageUp-Down cycle — all via the REUSED sidebar hooks (finding 50). The
  // section "+" calls `newTerminalInWorkspace` for a specific workspace. v2.5.6 also surfaces the
  // per-row rename/close callbacks.
  //
  // The FALLBACK workspace for a Ctrl+T that cannot inherit a workspace from the selection: the
  // active project's ROOT (else its first) workspace. v2.5.6 finding #26 forbids unattached
  // terminals, so Ctrl+T attaches here instead of spawning a loose terminal (only a project with
  // NO workspaces leaves this null → the hook's last-resort loose fallback).
  const fallbackWorkspace = useMemo(() => {
    const ws = workspaces.find((w) => w.is_root) ?? workspaces[0] ?? null;
    return ws ? { id: ws.id, path: ws.path } : null;
  }, [workspaces]);

  // THE PANE CHORDS, registered by the ONE `useTerminalShortcuts` call `useTerminalActions` owns
  // (a second registration would double-bind — and double-fire — Ctrl+T / Ctrl+W).
  //
  // They need the tiling controller, which is created BELOW because it needs the very
  // `createTerminalInWorkspace` this call returns. A ref closes that loop: the handlers are stable
  // and read the LIVE controller, and a chord can only fire after the render that filled the ref.
  const tilingRef = useRef<PaneLayoutController | null>(null);
  const paneShortcuts = useMemo<PaneShortcutHandlers>(() => {
    const split = (direction: "row" | "column") => () => {
      const tiling = tilingRef.current;
      if (tiling?.focusedPaneId != null) tiling.splitPane(tiling.focusedPaneId, direction);
    };
    return {
      onSplitRight: split("row"),
      onSplitDown: split("column"),
      onFocusLeft: () => tilingRef.current?.focusNeighborPane("left"),
      onFocusRight: () => tilingRef.current?.focusNeighborPane("right"),
      onFocusUp: () => tilingRef.current?.focusNeighborPane("up"),
      onFocusDown: () => tilingRef.current?.focusNeighborPane("down"),
      onClosePane: () => tilingRef.current?.closeFocusedPane(),
    };
  }, []);

  const {
    newTerminalInWorkspace,
    createTerminalInWorkspace,
    closeTerminal,
    renameTerminal,
    reorderTerminals,
    orderedTerminalIds,
  } = useTerminalActions(allTerminals, fallbackWorkspace, paneShortcuts);

  // WHICH workspaces actually ANSWERED the current runtime read — `useProjectRuntime` silently
  // omits a rejected `workspace_runtime_snapshot`, so `allTerminals` may be PARTIAL rather than
  // exhaustive. Handing the set in is what lets the tiling tell "this terminal is gone" from
  // "nobody answered for it" instead of pruning live panes (see `RuntimeCoverage`).
  const resolvedWorkspaceIds = useMemo(
    () => new Set(runtimeByWorkspace.keys()),
    [runtimeByWorkspace],
  );

  // THE TILING: which terminals are on screen, where, and which pane is focused. It owns the
  // split (create the terminal FIRST, place the leaf only on success), the restore of this
  // project's persisted layout, and the reconcile against the live terminal set.
  const tiling = usePaneLayout({
    projectId,
    terminals: allTerminals,
    workspaces,
    // The split creates its terminal through the SAME sidebar CRUD as the "+" — minus the select,
    // which the split owns (it focuses the new pane itself).
    createTerminal: createTerminalInWorkspace,
    resolvedWorkspaceIds,
  });
  tilingRef.current = tiling;

  // CLOSE EVERY TERMINAL OF A GROUP — the panel's destructive twin of "Ungroup all" (human review
  // 01M1312M70TP8TJWT4TG505S0J). It lives HERE rather than in `usePaneLayout` because it is the one
  // group gesture that is NOT a layout edit: `usePaneLayout` owns the collection and never touches a
  // terminal record, while this closes real terminals through the SAME `closeTerminal` the row's ×
  // and Ctrl+W already use — same drop, same persisted `closed`, same selection re-target, and
  // therefore the same compaction of the tiling, driven by the existing reconcile instead of by a
  // second path. The group needs no explicit dissolution either: it loses its members one by one
  // and the model's own rule ("a group holds at least two") drops it on the way through.
  //
  // SEQUENTIAL, deliberately: each close awaits its predecessor, so the snapshot and the selection
  // settle one step at a time rather than racing one reconcile against three removals.
  const closeGroup = useMemo(
    () => (groupId: string) => {
      const group = tilingRef.current?.groups.find((candidate) => candidate.id === groupId);
      if (group == null) return;
      const members = groupMemberIds(group);
      void (async () => {
        for (const terminalId of members) await closeTerminal(terminalId);
      })();
    },
    [closeTerminal],
  );

  // IDLE-SESSION UNLOAD (v2.5.5): on a timer, ask the sidecar to unload DORMANT agent sessions to
  // free resources — never a VISITED terminal, and never an ACTIVE one (the sidecar enforces
  // that). A revisit re-spawns + resumes via the phase-2 lazy park+inject.
  //
  // THE VISITED SET IS EVERY VISIBLE PANE, not the selection. With the tiling, four terminals can
  // be on screen while ONE holds the focus; protecting only the focused one would let the sidecar
  // unload the sessions of three panes the user is looking at. That is a correctness condition,
  // not an optimisation — hence `visibleTerminalIds` (every leaf of the tree).
  useIdleUnload(tiling.visibleTerminalIds);

  // THE COMMAND GROUPS of the project — the DB-backed twin of the terminal groups (an agent drives
  // the very same operations over MCP, and MCP cannot see the renderer's `localStorage`). Mounted
  // ONCE here, next to the tiling, and published to the sidebar + the settings screen through
  // `<CommandGroupsProvider>`; it re-reads on `commands://changed`, which both the IPC routes and
  // the MCP tool broadcast, so an agent's grouping repaints with no polling.
  const commandGroups = useCommandGroups(projectId);

  // INERT e2e seam (`window.__nyxReorderTerminals`), GATED behind VITE_NYX_E2E=1 (tree-shaken out
  // of a production build). Exposes the SAME within-group terminal reorder the drag commits
  // (`reorderTerminals` → `reorder` IPC → persisted `order_index`). The reorder gate drives it
  // because the WebDriver Actions pointer API is unusable in this Electron harness
  // (`Browser.getWindowForTarget` unsupported), so a simulated drag never fires the dnd-kit sensor;
  // the spec still asserts the DOM row order actually changed AND survived a real relaunch (honest
  // persistence), so it fails if the product reorder regresses.
  useEffect(() => {
    if (import.meta.env.VITE_NYX_E2E !== "1") return;
    (
      window as unknown as {
        __nyxReorderTerminals?: (group: string | null, ids: string[]) => void;
      }
    ).__nyxReorderTerminals = (group, ids) => reorderTerminals(group, ids);
    return () => {
      delete (window as unknown as { __nyxReorderTerminals?: unknown }).__nyxReorderTerminals;
    };
  }, [reorderTerminals]);

  // DEEP-LINK: when the URL carries `?terminal=<id>` (the Runtime tab opened a precise terminal),
  // select THAT terminal as soon as it appears in the project's runtime — overriding the
  // auto-select-first below. Applied ONCE per distinct target id (a ref guards it) so the user
  // can freely select another terminal afterwards without being yanked back.
  const appliedDeepLinkRef = useRef<string | null>(null);
  const deepLinkExists =
    deepLinkTerminalId != null && allTerminals.some((t) => t.id === deepLinkTerminalId);
  useEffect(() => {
    if (
      deepLinkTerminalId != null &&
      deepLinkExists &&
      appliedDeepLinkRef.current !== deepLinkTerminalId
    ) {
      appliedDeepLinkRef.current = deepLinkTerminalId;
      select({ kind: "terminal", id: deepLinkTerminalId });
    }
  }, [deepLinkTerminalId, deepLinkExists, select]);

  // The MIRROR of the terminal deep-link, for a managed COMMAND: when the URL carries
  // `?command=<id>` (the Runtime tab opened a precise command's detail), select THAT service as
  // soon as it appears in the project's runtime — so its live output pane shows. Applied ONCE per
  // distinct id (a ref guards it) so the user can freely select elsewhere afterwards.
  const appliedCommandDeepLinkRef = useRef<string | null>(null);
  const commandDeepLinkExists =
    deepLinkCommandId != null && allServices.some((s) => s.id === deepLinkCommandId);
  useEffect(() => {
    if (
      deepLinkCommandId != null &&
      commandDeepLinkExists &&
      appliedCommandDeepLinkRef.current !== deepLinkCommandId
    ) {
      appliedCommandDeepLinkRef.current = deepLinkCommandId;
      select({ kind: "service", id: deepLinkCommandId });
    }
  }, [deepLinkCommandId, commandDeepLinkExists, select]);

  // Auto-focus a terminal on entry so a pane shows without a click: RESTORE this project's LAST
  // active terminal (persisted, so a relaunch / return to Terminals reopens it) when it still
  // exists in the live runtime, else the FIRST terminal — the guard against a stale id (a
  // terminal closed while away) is the existence check, which falls back cleanly. A no-op once
  // the user has selected anything (never overriding a live selection); re-runs only when the
  // terminal set changes. SUPPRESSED while a deep-link is outstanding (present but not yet
  // applied, or still pending its terminal) so it never races the deep-link onto the first
  // terminal; once the deep-link has been applied it is allowed again (harmless — a selection
  // already exists by then).
  const firstTerminalId = allTerminals[0]?.id ?? null;
  const deepLinkOutstanding =
    (deepLinkTerminalId != null && appliedDeepLinkRef.current !== deepLinkTerminalId) ||
    (deepLinkCommandId != null && appliedCommandDeepLinkRef.current !== deepLinkCommandId);
  useEffect(() => {
    if (selected != null || firstTerminalId == null || deepLinkOutstanding) return;
    const remembered = loadLastTerminalId(projectId);
    const restore =
      remembered != null && allTerminals.some((t) => t.id === remembered)
        ? remembered
        : firstTerminalId;
    select({ kind: "terminal", id: restore });
  }, [selected, firstTerminalId, deepLinkOutstanding, projectId, allTerminals, select]);

  // Remember the selected TERMINAL as this project's last active terminal, so the restore above
  // reopens it after a relaunch or a return to the page. Only terminals are persisted (a selected
  // service is a transient watch surface); the provider is keyed by project, so `projectId` is
  // stable for this instance and the write always lands under the right project.
  useEffect(() => {
    if (selected?.kind === "terminal") saveLastTerminalId(projectId, selected.id);
  }, [selected, projectId]);

  return (
    // Thread the per-row RENAME + CLOSE callbacks to every terminal row (in the workspace tree)
    // via context — the reused `use-terminal-rename` provider + the sibling close provider — so a
    // row's inline rename / "×" acts WITHOUT prop-drilling through the accordion/tree chain.
    <TerminalRenameProvider rename={renameTerminal}>
      <TerminalCloseProvider close={closeTerminal}>
        {/* DRAG-REORDER wiring (v2.5.6): threads the group reorder callback + the live global
            order to every terminal row (in the workspace tree) via context, so a drag persists
            (backend `reorder` → `order_index`) and the tree repaints in the new order without
            prop-drilling. */}
        <TerminalReorderProvider
          reorderTerminals={reorderTerminals}
          orderedIds={orderedTerminalIds}
        >
          {/* THE GROUPS, PUBLISHED TO THE SIDEBAR. The block treatment and its four drag gestures
              are drawn by `<SortableTerminalRows>` deep inside the workspace accordion, and every
              mutator below is one of `usePaneLayout`'s: the collection has exactly ONE owner, and
              this provider is a view onto it, never a second copy. Its command twin publishes the
              project's `managed_command_groups` to the COMMANDS section of the same tree, so both
              lists wear the same head over the same panel. */}
          <CommandGroupsProvider controller={commandGroups}>
            <TerminalGroupsProvider
              groups={tiling.groups}
              // ACTIVATING A FOLDER LINE (human review 01M14112THEB4094B2G614SXR5) — the tiling's
              // own `showGroup` and the group it is already showing, threaded so the sidebar's
              // folder can be selected exactly like a member row, with no second navigation rule.
              showGroup={tiling.showGroup}
              shownGroupId={tiling.showingGroupId}
              leaveGroup={tiling.leaveGroup}
              reorderGroupMembers={tiling.reorderGroupMembers}
              insertIntoGroup={tiling.insertIntoGroup}
              canInsertIntoGroup={tiling.canInsertIntoGroup}
              createGroupWith={tiling.createGroupWith}
              canCreateGroup={tiling.canCreateGroup}
              dissolveGroup={tiling.dissolveGroup}
              closeGroup={closeGroup}
              renameGroup={tiling.renameGroup}
              recolorGroup={tiling.recolorGroup}
            >
              {/* Full height UNDER the chrome bar: `h-app`, never a viewport-height utility —
                  the shell root already spends one `--chrome-bar-h` (see `app-height.guard`). */}
              <div className="flex h-app bg-background text-foreground">
                <SidebarShell
                  aria-label="Workspaces"
                  header={
                    <div className="flex items-start justify-between gap-2">
                      <div className="min-w-0">
                        <h1
                          className="truncate font-semibold text-sm text-foreground"
                          title={projectName}
                        >
                          {projectName}
                        </h1>
                        <p className="text-2xs uppercase tracking-wide text-muted-foreground">
                          Terminals
                        </p>
                      </div>
                      <div className="flex shrink-0 items-center gap-1.5">
                        {/* Project settings (gear) — the VISIBLE, discoverable entry point to the
                            project's settings (the Global pane's `resume_agent_sessions` toggle).
                            It surfaces in the active project's header the SAME
                            `<ProjectSettingsDialog>` the rail tile's right-click "Project settings"
                            opens (base-layout), so the reglage is findable instead of hidden
                            behind a context menu. */}
                        <button
                          type="button"
                          onClick={onOpenProjectSettings}
                          className="flex size-7 shrink-0 items-center justify-center rounded-md border border-border/70 text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground"
                          aria-label="Project settings"
                          title="Project settings"
                        >
                          <SettingsIcon className="size-4" />
                        </button>
                        {/* Add a WORKSPACE (folder) to the entered project — the existing folder-picker →
                  name-dialog → create_workspace flow, surfaced here in the current shell (the
                  capability existed but was only wired into the removed sidebar). */}
                        <button
                          type="button"
                          onClick={onAddWorkspace}
                          className="flex size-7 shrink-0 items-center justify-center rounded-md border border-dashed border-border/70 text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground"
                          aria-label="Add workspace"
                          title="Add workspace"
                        >
                          <FolderPlusIcon className="size-4" />
                        </button>
                      </div>
                    </div>
                  }
                >
                  {/* The workspace accordions fill the sidebar; they scroll when the content grows past
                the viewport. (v2.5.6 finding #26 removed the "loose terminals" foot band — no
                terminal is surfaced unattached to a workspace anymore.) */}
                  <div className="flex min-h-full flex-col">
                    <nav className="flex-1">
                      {workspaces.length === 0 ? (
                        <p className="px-4 py-3 text-sm italic text-muted-foreground/70">
                          No workspaces in this project.
                        </p>
                      ) : (
                        workspaces.map((workspace, index) => (
                          <WorkspaceAccordion
                            key={workspace.id}
                            workspace={workspace}
                            snapshot={runtimeByWorkspace.get(workspace.id) ?? null}
                            defaultOpen={index === 0}
                            onNewTerminal={() =>
                              void newTerminalInWorkspace(workspace.id, workspace.path)
                            }
                            onNewCommand={() =>
                              onOpenCommands({ id: workspace.id, path: workspace.path })
                            }
                            // The workspace's OWN lifecycle, on its own line: rename any workspace,
                            // remove a non-root one (the header's kebab disables it on the root
                            // with its reason — the backend refuses that removal).
                            onRename={onRenameWorkspace}
                            onRemove={onRemoveWorkspace}
                          />
                        ))
                      )}
                    </nav>
                  </div>
                </SidebarShell>

                <main className="min-w-0 flex-1">
                  <RuntimePaneHost
                    terminals={allTerminals}
                    services={allServices}
                    tiling={tiling}
                  />
                </main>
              </div>
            </TerminalGroupsProvider>
          </CommandGroupsProvider>
        </TerminalReorderProvider>
      </TerminalCloseProvider>
    </TerminalRenameProvider>
  );
}

export default TerminalsPage;
