import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { HouseIcon } from "lucide-react";
import { nyxBridge } from "@/bridge";

import { Button } from "@/components/ui/button";
import { SidebarShell } from "@/components/ui/sidebar-shell";
import { Spinner } from "@/components/ui/spinner";
import { CommandGroupsProvider } from "@/components/command/command-groups-context";
import { useCommandGroups } from "@/components/command/use-command-groups";
import { useAgentSessions } from "@/components/sidebar/use-agent-sessions";
import { TerminalRenameProvider } from "@/components/sidebar/use-terminal-rename";
import { TerminalStatsProvider } from "@/components/sidebar/use-terminal-stats";
import type { TerminalRecord } from "@/components/sidebar/use-terminals";
import { ConsumedNotificationsProvider } from "@/components/terminals/use-consumed-notifications";
import { RuntimePaneHost } from "@/components/terminals/runtime-pane-host";
import {
  RuntimeSelectionProvider,
  useRuntimeSelection,
} from "@/components/terminals/runtime-selection";
import { loadLastTerminalId, saveLastTerminalId } from "@/components/terminals/ui-persistence";
import { usePaneLayout, type PaneLayoutController } from "@/components/terminals/use-pane-layout";
import { useProjectRuntime } from "@/components/terminals/use-project-runtime";
import {
  useTerminalActions,
  type PaneShortcutHandlers,
} from "@/components/terminals/use-terminal-actions";
import { TerminalCloseProvider } from "@/components/terminals/use-terminal-close";
import { TerminalGroupsProvider } from "@/components/terminals/use-terminal-groups";
import { TerminalReorderProvider } from "@/components/terminals/use-terminal-reorder";
import { WorkspaceRuntimeTree } from "@/components/terminals/workspace-runtime-tree";
import { groupMemberIds } from "@/components/panes/pane-groups";
import type {
  WorkspaceRuntimeService,
  WorkspaceRuntimeSnapshot,
  WorkspaceRuntimeTerminal,
} from "@/components/runtime/use-workspace-runtime";
import { SystemCommandsDialog } from "./system-commands-dialog";
import { SYSTEM_DEFAULT_CWD_LABEL, systemCwdLabel } from "./system-home-path";
import { useSystemScope, type SystemScopeDetail } from "./use-system-scope";

/**
 * `<SystemScopePage>` — the SYSTÈME scope's ONE screen (PRD « Scope Système », phase 3).
 *
 * ## What it is
 *
 * The cross-project terminals and commands that belong to no code project: the "palbank services"
 * and "docker · postgres" the user kept outside nyx because everything here lived under
 * project → workspace. It is reached from the tile in the rail's FOOT GROUP, above Settings, and it
 * is a PAGE OF ITS OWN — the `/system` route (human review `01M1EC7Y2625Q07C5386GKHZFS`, finding
 * U1).
 *
 * ## A page, not a scope
 *
 * It used to render in the index route's slot, switched by a `systemScope` flag that ALSO replaced
 * the project context: the entered project was de-selected, Work / Knowledge / Engine / Catalogue
 * were masked, and any project-scoped route redirected here. The user's verdict on the real app was
 * that losing one's project to consult a page makes no sense — so the page took a path, and
 * everything the flag did with the project went with the flag. The entered project stays entered
 * while this screen is up; the only thing this page owns is its own column.
 *
 * ## Two reads, composed HERE
 *
 * IDENTITY comes from `system_scope_detail` (`useSystemScope`): the technical project, its ONE
 * primary workspace, and the absolute home. RUNTIME comes from `workspace_runtime_snapshot` over
 * that workspace, through the very `useProjectRuntime` every project page uses. Phase 2 left the
 * composition open on purpose and this is the answer: the dedicated read stays an IDENTITY read —
 * adding the aggregate to it would have minted a second runtime read model, for one workspace, kept
 * live by a second subscription, drifting from the one the whole tree already reads.
 *
 * ## What is DELIBERATELY absent
 *
 * No workspace band: the Système workspace is an identity of attachment, not a folder one navigates
 * — showing `~` as a band above sections that are all `~` teaches nothing. No project-settings gear,
 * no "add workspace", no rename, no delete: the backend refuses all four on the singleton
 * (`require_user_project` / `require_user_workspace`), and an affordance that can only error is
 * worse than no affordance. Sections are Terminals THEN Commands — the canonical order of every
 * other runtime tree, unchanged.
 *
 * ## The cwd is `~` on screen and ABSOLUTE on the wire
 *
 * Every Système terminal and command runs at the platform home. The foot names that default as `~`
 * (with the real path one hover away) and an unnamed terminal row reads `~` instead of a raw
 * `C:/Users/…`. Nothing converts the other way: `create_system_terminal` takes no path at all — the
 * use case reads the home off the bootstrapped scope — so a `~` can never reach the backend.
 */
export function SystemScopePage() {
  const { scope, loading, error, refresh } = useSystemScope();

  // LOADING — the first read has not answered yet. The shell (rail + chrome) is already on screen
  // around this outlet, so the page only needs to say "not yet" without flashing an empty tree.
  if (loading && scope == null) {
    return (
      <div
        className="flex h-app w-full items-center justify-center bg-background"
        aria-busy="true"
        aria-label="Loading the System scope"
      >
        <Spinner className="size-6 text-muted-foreground" />
      </div>
    );
  }

  // ERROR — the read FAILED. The backend's own wording is shown (a boot that refused an incoherent
  // scope says exactly why), with the one action that can change the outcome.
  if (error != null && scope == null) {
    return (
      <div className="flex h-app w-full flex-col items-center justify-center gap-3 bg-background px-6 text-center">
        <h1 className="font-semibold text-lg text-foreground">System</h1>
        <p role="alert" className="max-w-md text-sm text-destructive">
          {error}
        </p>
        <Button variant="outline" size="sm" onClick={() => void refresh()}>
          Retry
        </Button>
      </div>
    );
  }

  // EMPTY — a legitimate `null`: the database has no singleton (it never went through `Db::open`).
  // A read reports the absence rather than bootstrapping behind the caller's back, so the page says
  // so instead of pretending to be an empty scope.
  if (scope == null) {
    return (
      <div className="flex h-app w-full flex-col items-center justify-center gap-3 bg-background px-6 text-center">
        <h1 className="font-semibold text-lg text-foreground">System</h1>
        <p className="max-w-md text-sm text-muted-foreground">
          The System scope is not available on this database yet. It is created at startup.
        </p>
      </div>
    );
  }

  return <SystemScopeLoaded scope={scope} />;
}

/** The page once the scope is known: the runtime read, the providers, the two columns. */
function SystemScopeLoaded({ scope }: { scope: SystemScopeDetail }) {
  const workspaceIds = useMemo(() => [scope.workspace.id], [scope.workspace.id]);
  const { runtimeByWorkspace } = useProjectRuntime(workspaceIds);
  const snapshot = runtimeByWorkspace.get(scope.workspace.id) ?? null;

  // The command MANAGER (create / import / edit / delete / group). Opened by the Commands section's
  // gear — the only composition surface of the scope, and the reason no project-settings modal is
  // mounted here at all.
  const [commandsOpen, setCommandsOpen] = useState(false);

  return (
    // ONE `terminal://stats` subscription for the page, outside the selection provider (same shape
    // as the Terminals page) so the rows read live CPU%/RAM on hover.
    <TerminalStatsProvider>
      <RuntimeSelectionProvider key={scope.project.id}>
        <ConsumedNotificationsProvider>
          <SystemScopeBody
            scope={scope}
            snapshot={snapshot}
            onOpenCommands={() => setCommandsOpen(true)}
          />
        </ConsumedNotificationsProvider>
      </RuntimeSelectionProvider>
      <SystemCommandsDialog
        open={commandsOpen}
        projectId={scope.project.id}
        home={scope.home}
        onClose={() => setCommandsOpen(false)}
      />
    </TerminalStatsProvider>
  );
}

/**
 * The composed layout, under the selection provider (the tiling and the auto-select read it).
 * Mirrors `<TerminalsBody>` minus everything the scope does not have — the workspace accordion,
 * the add-workspace flow, the project-settings modal and the lazy agent resume (whose ONE call
 * site is the project page, by design: a Système terminal hosts no agent session).
 */
function SystemScopeBody({
  scope,
  snapshot,
  onOpenCommands,
}: {
  scope: SystemScopeDetail;
  /** The Système workspace's live aggregate, or `null` until the first snapshot lands. */
  snapshot: WorkspaceRuntimeSnapshot | null;
  /** Open the command manager (the Commands section's gear). */
  onOpenCommands: () => void;
}) {
  const { selected, select } = useRuntimeSelection();
  const { byActivity } = useAgentSessions();
  const projectId = scope.project.id;
  const workspaceId = scope.workspace.id;

  const terminals = useMemo<readonly WorkspaceRuntimeTerminal[]>(
    () => snapshot?.terminals ?? [],
    [snapshot],
  );
  const services = useMemo<readonly WorkspaceRuntimeService[]>(
    () => snapshot?.services ?? [],
    [snapshot],
  );

  /**
   * CREATE A SYSTÈME TERMINAL — the ATOMIC use case, and nothing else.
   *
   * `create_system_terminal` writes the record with the absolute home as its cwd, the Système
   * workspace as its `workspace_id` and a MANUAL binding in ONE transaction, then opens the shell
   * after the commit. It takes no path and no workspace: the singleton resolves both itself, which
   * is exactly why a `~` can never travel to the backend from this page. The default
   * create-then-attach pair the project page uses is NOT usable here — it would leave the terminal
   * unattached for a round-trip, i.e. inside the very orphan bucket this scope must stay disjoint
   * from.
   */
  const createSystemTerminal = useCallback(async () => {
    const row = await nyxBridge.invoke<TerminalRecord>("create_system_terminal", {});
    return row.id;
  }, []);

  // The pane chords need the tiling, which needs the creator this call returns — the same ref knot
  // the Terminals page unties, untied the same way.
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

  const fallbackWorkspace = useMemo(
    () => ({ id: workspaceId, path: scope.home }),
    [workspaceId, scope.home],
  );

  const {
    newTerminalInWorkspace,
    createTerminalInWorkspace,
    closeTerminal,
    renameTerminal,
    reorderTerminals,
    orderedTerminalIds,
  } = useTerminalActions(terminals, fallbackWorkspace, paneShortcuts, createSystemTerminal);

  const resolvedWorkspaceIds = useMemo(
    () => new Set(snapshot != null ? [workspaceId] : []),
    [snapshot, workspaceId],
  );

  const tiling = usePaneLayout({
    projectId,
    terminals,
    workspaces: [scope.workspace],
    createTerminal: createTerminalInWorkspace,
    resolvedWorkspaceIds,
  });
  tilingRef.current = tiling;

  // Close every terminal of a group — the same sequential drop the Terminals page owns (each close
  // awaits its predecessor so one reconcile settles before the next removal).
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

  // The scope's command GROUPS (`managed_command_groups` of the singleton project) — the same
  // DB-backed controller the project page mounts, re-read on `commands://changed`, so a grouping
  // made in the manager repaints the sidebar with no reload.
  const commandGroups = useCommandGroups(projectId);

  // NO `Clear output` CONTROL ON THIS PAGE (human review `01M1EC7Y2625Q07C5386GKHZFS`, finding U2).
  // Phase 3 put an eraser on the Système command rows — the human twin of the MCP
  // `clear_command_output` — because the singleton is where a hand at the keyboard owns every verb
  // of a command. Seen in the real app, the user judged the verb INCOHERENT: the same row exists on
  // every project surface and only this one offered it. Offered the choice between generalising it
  // and dropping it, they chose to drop it. So the page threads no clear callback, and
  // `<WorkspaceRuntimeTree>` has no clear prop left to thread.
  //
  // The IPC `command_clear_output` is deliberately UNTOUCHED (`bridge/contract.ts`): it is on the
  // Système guard matrix and in the gate, and its removal was explicitly out of the finding's
  // perimeter. It has no renderer caller as a result — that state was flagged, and review
  // `01M1EGR1777881EQ59MWV6QYYZ` (V1) settled it: the IPC is KEPT as the human twin of the MCP tool.

  // Auto-focus a terminal on entry (restore the scope's last one, else the first), and remember the
  // selected terminal — the SAME persistence the project page uses, keyed by the singleton's own
  // project id, so returning to System reopens where the user left off. A no-op once anything is
  // selected: the user's own choice is never overridden.
  const firstTerminalId = terminals[0]?.id ?? null;
  useEffect(() => {
    if (selected != null || firstTerminalId == null) return;
    const remembered = loadLastTerminalId(projectId);
    const restore =
      remembered != null && terminals.some((t) => t.id === remembered)
        ? remembered
        : firstTerminalId;
    select({ kind: "terminal", id: restore });
  }, [selected, firstTerminalId, projectId, terminals, select]);
  useEffect(() => {
    if (selected?.kind === "terminal") saveLastTerminalId(projectId, selected.id);
  }, [selected, projectId]);

  // The `~` rendering of the scope's home, bound once for the rows that fall back to a cwd.
  const formatCwd = useCallback((cwd: string) => systemCwdLabel(cwd, scope.home), [scope.home]);

  return (
    <TerminalRenameProvider rename={renameTerminal}>
      <TerminalCloseProvider close={closeTerminal}>
        <TerminalReorderProvider
          reorderTerminals={reorderTerminals}
          orderedIds={orderedTerminalIds}
        >
          <CommandGroupsProvider controller={commandGroups}>
            <TerminalGroupsProvider
              groups={tiling.groups}
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
              {/* `h-app` — the usable height UNDER the chrome bar, never a viewport unit (the shell
                  root already spends one `--chrome-bar-h`; see `app-height.guard`). */}
              <div className="flex h-app bg-background text-foreground">
                <SidebarShell
                  aria-label="System"
                  header={
                    /* THE NAME, SAID ONCE. A `system` badge used to sit beside this title, back
                       when Système was a SCOPE one entered in the index route's slot and the badge
                       told a scope apart from a project in the same header. Système is a routed
                       PAGE since finding U1, so the page's name IS its nature and the badge only
                       said `System` a second time, an inch from the first (finding X1).

                       AND THE SLOT STAYS EMPTY. Where a project's header carries its settings gear
                       and its add-workspace button, this one carries neither — the backend refuses
                       both on the singleton. The vacancy is the statement; nothing else moves in. */
                    <div className="min-w-0">
                      <h1 className="truncate font-semibold text-sm text-foreground">System</h1>
                      <p className="text-2xs uppercase tracking-wide text-muted-foreground">
                        Terminals
                      </p>
                    </div>
                  }
                  footer={
                    <p
                      className="flex items-center gap-1.5 font-mono text-2xs text-muted-foreground"
                      title={scope.home}
                    >
                      <HouseIcon aria-hidden className="size-3.5 shrink-0" />
                      <span>default cwd</span>
                      <span className="text-foreground">{SYSTEM_DEFAULT_CWD_LABEL}</span>
                    </p>
                  }
                >
                  {/* NO WORKSPACE BAND — the sections are the first thing in the rail. Terminals
                      then Commands, the canonical order, from the SAME tree every workspace
                      renders. */}
                  <nav aria-label="System runtime" className="flex min-h-full flex-col">
                    {snapshot ? (
                      <WorkspaceRuntimeTree
                        snapshot={snapshot}
                        onNewTerminal={() => void newTerminalInWorkspace(workspaceId, scope.home)}
                        onNewCommand={onOpenCommands}
                        agentActivity={(terminalId) => byActivity.get(terminalId)}
                        formatCwd={formatCwd}
                      />
                    ) : (
                      <p className="px-4 py-2 text-sm italic text-muted-foreground/70">
                        Loading runtime…
                      </p>
                    )}
                  </nav>
                </SidebarShell>

                <main className="min-w-0 flex-1">
                  <RuntimePaneHost terminals={terminals} services={services} tiling={tiling} />
                </main>
              </div>
            </TerminalGroupsProvider>
          </CommandGroupsProvider>
        </TerminalReorderProvider>
      </TerminalCloseProvider>
    </TerminalRenameProvider>
  );
}

export default SystemScopePage;
