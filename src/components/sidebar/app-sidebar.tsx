import { useMemo } from "react";
import { FolderPlusIcon, PlusIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Tooltip } from "@/components/ui/tooltip";
import { ProjectItem } from "./project-item";
import { SelectionRail, useActiveRail } from "./active-rail";
import { SortableTerminalList } from "./sortable-terminal-list";
import type { CommandRecord, ProjectTree, WorkspaceRecord } from "./use-projects";
import type { TerminalRecord } from "./use-terminals";

/**
 * The selection-rail KEY: the id of whichever row is active — a terminal
 * (`activeId`) OR a command (`activeCommandId`) — so a glide is triggered on every
 * selection change, including command→command.
 *
 * Why this exists: the sidebar's `activeId` is forced to `null` while a command is
 * active (see `terminal-manager.tsx`), so keying the rail on `activeId` ALONE kept
 * the key `null` across a command→command switch and the rail never re-glided
 * (review 01KV6F1B…). Falling back to `activeCommandId` makes the key change on a
 * command switch too. Exported so the precedence is unit-tested.
 */
export function railKey(
  activeId: string | null,
  activeCommandId: string | null | undefined,
): string | null {
  return activeId ?? activeCommandId ?? null;
}

export interface AppSidebarProps {
  /** Projects with their workspaces (root first), in creation order. */
  projects: ProjectTree[];
  /** All alive terminals (grouped here by `workspace_id`). */
  terminals: TerminalRecord[];
  /** Record id of the globally-active terminal, or null. */
  activeId: string | null;
  /** Live record→PTY id map for the auto label. */
  ptyIds?: Map<string, number | null>;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  /** Launch a new terminal in `workspace` (cwd = workspace.path). */
  onNewTerminal: (workspace: WorkspaceRecord) => void;
  /** Open an UNATTACHED (loose) terminal (no project/workspace, mode auto). */
  onNewLooseTerminal: () => void;
  /** Create a new project (opens the create modal: folder pick → name). */
  onAddProject: () => void;
  /** Add a workspace to an existing project (folder picker / name flow). */
  onAddWorkspace: (tree: ProjectTree) => void;
  /** Open the edit (rename) modal for a project. */
  onEditProject?: (tree: ProjectTree) => void;
  /** Open the delete-confirm modal for a project. */
  onDeleteProject?: (tree: ProjectTree) => void;
  /** Open the "Manage commands" modal for a project (PRD-3). */
  onManageCommands?: (tree: ProjectTree) => void;
  /**
   * Commands (PRD-3 instances) grouped by `workspace_id`. Empty/absent until a
   * project has command templates. Threaded to each workspace's subsections.
   */
  commandsByWorkspace?: Map<string, CommandRecord[]>;
  /** Record id of the active command (drives the shared selection rail). */
  activeCommandId?: string | null;
  /** Select a command row → mount its `<CommandView>` in the main pane. */
  onSelectCommand?: (id: string) => void;
  /** Persist a new order for a workspace's terminals (within-workspace only). */
  onReorderTerminals?: (workspaceId: string, ids: string[]) => void;
  /**
   * Persist a new order for the LOOSE (unattached) terminals — the dragged id
   * sequence for the top-level TERMINALS section (`workspace_id == null`).
   * Optional so the sidebar still renders without reorder wiring (isolation
   * tests).
   */
  onReorderLooseTerminals?: (ids: string[]) => void;
  /** Persist a project band's open/closed state (restored on reload). */
  onSetProjectCollapsed?: (id: string, collapsed: boolean) => void;
  /** Persist a workspace band's open/closed state (restored on reload). */
  onSetWorkspaceCollapsed?: (id: string, collapsed: boolean) => void;
  className?: string;
}

/**
 * `<AppSidebar>` — the WHOLE app sidebar, re-aligned to the elected v6 prototype.
 * It renders:
 *
 *  - a sidebar HEAD with the Nyx wordmark + a GLOBAL new-terminal `+` that opens a
 *    LOOSE (unattached) terminal;
 *  - the scrollable **PROJECTS** section: a sticky band label + one `<ProjectItem>`
 *    per project (full-bleed band header, project dot, collapsed "N" summary,
 *    hover kebab), expanding into workspaces → typed Terminals/Commands;
 *  - a pinned **TERMINALS** footer band listing the loose terminals (no
 *    `workspace_id`); drag-reorderable, with the same enter/exit row animations.
 *
 * The head `+`, the Projects band's folder-add and the footer Terminals band's
 * `+` all sit on the SAME right edge (`px-3` + `icon-xs`) so they line up down
 * the right edge (finding 01KV3CP2SQMCZAAQYGNWCNMWKG).
 *
 * SELECTION RAIL: a single MEASURED `<SelectionRail>` bar (see `useActiveRail`)
 * lives in a `relative` rail HOST that spans the rows (the scroll section + the
 * footer). The hook reads the active row's box and glides the bar there on
 * selection, re-measuring on collapse / add / close / reorder / scroll / resize.
 * This replaced a Motion `layoutId` rail, which dnd-kit's mid-drag row remounts
 * kept desyncing — a measured bar only reads the DOM, so nothing dnd-kit does
 * to the rows can break it.
 */
export function AppSidebar({
  projects,
  terminals,
  activeId,
  ptyIds,
  onSelect,
  onClose,
  onNewTerminal,
  onNewLooseTerminal,
  onAddProject,
  onAddWorkspace,
  onEditProject,
  onDeleteProject,
  onManageCommands,
  commandsByWorkspace,
  activeCommandId,
  onSelectCommand,
  onReorderTerminals,
  onReorderLooseTerminals,
  onSetProjectCollapsed,
  onSetWorkspaceCollapsed,
  className,
}: AppSidebarProps) {
  // Group terminals by their workspace binding once per terminals change; the
  // unbound (loose) terminals are collected separately for the TERMINALS section.
  const { terminalsByWorkspace, looseTerminals } = useMemo(() => {
    const map = new Map<string, TerminalRecord[]>();
    const loose: TerminalRecord[] = [];
    for (const t of terminals) {
      const wsId = t.workspace_id;
      if (!wsId) {
        loose.push(t);
        continue;
      }
      const list = map.get(wsId);
      if (list) list.push(t);
      else map.set(wsId, [t]);
    }
    return {
      terminalsByWorkspace: map,
      looseTerminals: loose,
    };
  }, [terminals]);

  // The single measured selection rail: refs for the host (spans the rows) and the
  // bar itself; re-measures on selection / layout / scroll (see `useActiveRail`).
  // The rail key must follow whichever row is active — a terminal (`activeId`) OR a
  // command (`activeCommandId`). The sidebar's `activeId` is forced to `null` while
  // a command is active (see `terminal-manager.tsx`), so command→command would keep
  // the key `null` both times and never re-glide; falling back to `activeCommandId`
  // makes a command switch re-glide just like a terminal switch.
  const { hostRef, railRef } = useActiveRail(railKey(activeId, activeCommandId));

  return (
    <aside
      className={cn(
        "flex h-full w-64 shrink-0 flex-col border-r border-sidebar-border bg-sidebar",
        className,
      )}
    >
      {/* === HEAD: Nyx wordmark + global '+' (new loose/unattached terminal) ===
            `px-3` matches the Projects band + footer band so the three '+' align. */}
      <div className="flex items-center justify-between border-b border-sidebar-border px-3 py-2.5">
        <span className="text-sm font-semibold tracking-widest text-sidebar-foreground">Nyx</span>
        <Tooltip label="New terminal (unattached)">
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label="New terminal"
            onClick={onNewLooseTerminal}
          >
            <PlusIcon />
          </Button>
        </Tooltip>
      </div>

      {/* Rail HOST: a `relative` container spanning the rows so the single
            measured selection bar can be positioned over the active row. */}
      <div ref={hostRef} className="relative flex min-h-0 flex-1 flex-col">
        <SelectionRail railRef={railRef} />

        {/* === PROJECTS (scrollable): sticky band label + one band per project === */}
        <section className="flex min-h-0 flex-1 flex-col overflow-y-auto pb-2">
          <div className="sticky top-0 z-20 flex items-center justify-between border-b border-sidebar-border bg-sidebar px-3 py-1.5">
            <span className="text-xs font-semibold tracking-wider text-muted-foreground uppercase">
              Projects
            </span>
            <Tooltip label="Add project">
              <Button
                variant="ghost"
                size="icon-xs"
                aria-label="Add project"
                onClick={onAddProject}
              >
                <FolderPlusIcon />
              </Button>
            </Tooltip>
          </div>

          <ul className="flex flex-col">
            {projects.map((tree) => (
              <ProjectItem
                key={tree.project.id}
                tree={tree}
                terminalsByWorkspace={terminalsByWorkspace}
                activeId={activeId}
                ptyIds={ptyIds}
                onSelect={onSelect}
                onClose={onClose}
                onNewTerminal={onNewTerminal}
                onAddWorkspace={onAddWorkspace}
                onEditProject={onEditProject}
                onDeleteProject={onDeleteProject}
                onManageCommands={onManageCommands}
                commandsByWorkspace={commandsByWorkspace}
                activeCommandId={activeCommandId}
                onSelectCommand={onSelectCommand}
                onReorderTerminals={onReorderTerminals}
                onSetCollapsed={onSetProjectCollapsed}
                onSetWorkspaceCollapsed={onSetWorkspaceCollapsed}
              />
            ))}
            {projects.length === 0 && (
              <li className="px-3 py-6 text-center text-xs text-muted-foreground">
                No projects yet. Add one with the button above.
              </li>
            )}
          </ul>
        </section>

        {/* === TERMINALS (pinned footer): loose/unattached terminals ===
            The band uses `px-3` (matching the head + Projects band) so its '+'
            lands on the same right edge as the other two. */}
        <section className="shrink-0 border-t border-sidebar-border px-2 pt-2 pb-2">
          <div className="flex items-center justify-between px-1 py-1">
            <span className="text-xs font-semibold tracking-wider text-muted-foreground uppercase">
              Terminals
            </span>
            <Tooltip label="New terminal (unattached)">
              <Button
                variant="ghost"
                size="icon-xs"
                aria-label="New unattached terminal"
                onClick={onNewLooseTerminal}
              >
                <PlusIcon />
              </Button>
            </Tooltip>
          </div>
          {looseTerminals.length > 0 ? (
            // Drag-sortable loose list (dnd-kit): a closed loose row runs its
            // height-collapse exit and the survivors reflow up (same enter/exit as
            // the workspace lists).
            <SortableTerminalList
              terminals={looseTerminals}
              activeId={activeId}
              ptyIds={ptyIds}
              onSelect={onSelect}
              onClose={onClose}
              onReorder={onReorderLooseTerminals}
              className="flex flex-col"
            />
          ) : (
            <p className="px-2 py-1 text-xs text-muted-foreground/70 italic select-none">
              No terminals — + to start
            </p>
          )}
        </section>
      </div>
    </aside>
  );
}
