import { useEffect, useMemo, useState } from "react";
import { SquareTerminalIcon } from "lucide-react";

import { useProjects } from "@/components/sidebar/use-projects";
import { ProjectCommandsDialog } from "@/components/command/project-commands-dialog";
import { PagePlaceholder } from "./page-placeholder";
import { useActiveProject } from "./active-project";
import { useProjectRuntime } from "./use-project-runtime";
import { RuntimeSelectionProvider, useRuntimeSelection } from "./runtime-selection";
import { ConsumedNotificationsProvider } from "./use-consumed-notifications";
import { RuntimePaneHost } from "./runtime-pane-host";
import { WorkspaceAccordion } from "./workspace-accordion";
import { useTerminalActions } from "./use-terminal-actions";
import type { WorkspaceRuntimeService, WorkspaceRuntimeTerminal } from "./use-workspace-runtime";

/**
 * `<TerminalsPage>` — the index route (`/`). One ENTERS a PROJECT (from the left rail); this
 * page lists the entered project's WORKSPACES as an accordion and, per workspace, its live
 * runtime tree (terminals + commands; agents on their terminals — no 3rd category) plus a
 * shared main pane showing the selected terminal's live xterm (or a service's output).
 *
 * The workspace stays the runtime PIVOT in the DB (PDR-0008) but is no longer a screen — it
 * appears only as a grouping inside the entered project. The runtime is fed LIVE by the
 * PRESERVED backend read model (`workspace_runtime_snapshot`, re-pulled on `*://changed`
 * pushes with NO polling — see `use-project-runtime`); the real xterm (`<TerminalDeck>`) is
 * reused for the panes (no second terminal). No business logic in the renderer.
 */
export function TerminalsPage() {
  const { activeProjectId } = useActiveProject();
  const { projects } = useProjects();

  const activeTree = useMemo(
    () => projects.find((p) => p.project.id === activeProjectId) ?? null,
    [projects, activeProjectId],
  );
  const workspaces = activeTree?.workspaces ?? [];
  const workspaceIds = useMemo(() => workspaces.map((w) => w.id), [workspaces]);

  const { runtimeByWorkspace } = useProjectRuntime(workspaceIds);

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
    <RuntimeSelectionProvider>
      <ConsumedNotificationsProvider>
        <TerminalsBody
          projectId={activeTree.project.id}
          projectName={activeTree.project.name}
          workspaces={workspaces}
          runtimeByWorkspace={runtimeByWorkspace}
          allTerminals={allTerminals}
          allServices={allServices}
        />
      </ConsumedNotificationsProvider>
    </RuntimeSelectionProvider>
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
}: {
  projectId: string;
  projectName: string;
  workspaces: ReturnType<typeof useProjects>["projects"][number]["workspaces"];
  runtimeByWorkspace: ReturnType<typeof useProjectRuntime>["runtimeByWorkspace"];
  allTerminals: readonly WorkspaceRuntimeTerminal[];
  allServices: readonly WorkspaceRuntimeService[];
}) {
  const { selected, select } = useRuntimeSelection();

  // Restore the old shell's terminal management, recabled to the page selection: Ctrl+T (new
  // terminal in the SELECTED terminal's workspace, else loose), Ctrl+W (close selected), the
  // Ctrl+Tab / Ctrl+PageUp-Down cycle — all via the REUSED sidebar hooks (finding 50). The
  // section "+" calls `newTerminalInWorkspace` for a specific workspace.
  const { newTerminalInWorkspace } = useTerminalActions(allTerminals);

  // The "new command" gear opens the SHARED project command manager (`<ProjectCommandsDialog>` —
  // the exact modal the old sidebar gear opened), scoped to the clicked workspace for the import
  // tab + subfolder picker. `null` = closed.
  const [commandsFor, setCommandsFor] = useState<{ id: string; path: string } | null>(null);

  // Auto-focus the FIRST terminal once one exists and nothing is selected yet, so a pane shows
  // on entry without a click. A no-op once the user has selected anything, and re-runs only
  // when the terminal set changes (never overriding a live selection).
  const firstTerminalId = allTerminals[0]?.id ?? null;
  useEffect(() => {
    if (selected == null && firstTerminalId != null) {
      select({ kind: "terminal", id: firstTerminalId });
    }
  }, [selected, firstTerminalId, select]);

  return (
    <div className="grid h-svh grid-cols-[260px_1fr] bg-background text-foreground">
      <aside className="flex h-svh flex-col border-r border-border/60 bg-muted/40">
        <div className="border-b border-border/60 px-3 py-3">
          <h1 className="truncate font-semibold text-sm text-foreground" title={projectName}>
            {projectName}
          </h1>
          <p className="text-2xs uppercase tracking-wide text-muted-foreground">Terminals</p>
        </div>
        <nav className="min-h-0 flex-1 overflow-y-auto">
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
                onNewTerminal={() => void newTerminalInWorkspace(workspace.id, workspace.path)}
                onNewCommand={() => setCommandsFor({ id: workspace.id, path: workspace.path })}
              />
            ))
          )}
        </nav>
      </aside>

      <main className="min-w-0">
        <RuntimePaneHost terminals={allTerminals} services={allServices} />
      </main>

      {/* The shared command manager modal (create/edit commands), opened by a Commands-section
          gear. Reuses the old sidebar's `<ProjectCommandsDialog>` verbatim. */}
      <ProjectCommandsDialog
        open={commandsFor !== null}
        projectId={projectId}
        projectName={projectName}
        importWorkspaceId={commandsFor?.id ?? null}
        workspacePath={commandsFor?.path ?? null}
        onClose={() => setCommandsFor(null)}
      />
    </div>
  );
}

export default TerminalsPage;
