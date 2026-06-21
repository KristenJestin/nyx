import { useState } from "react";
import {
  ChevronRightIcon,
  FolderPlusIcon,
  MoreVerticalIcon,
  PencilIcon,
  TerminalSquareIcon,
  Trash2Icon,
} from "lucide-react";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Menu, MenuItem, MenuSeparator } from "@/components/ui/menu";
import { CollapsibleSection } from "./collapsible-section";
import { WorkspaceItem } from "./workspace-item";
import { itemTransition } from "./item-motion";
import { showWorkspaceSection, workspaceDisplayLabel } from "./project-item.utils";
import type { CommandRecord, ProjectTree, WorkspaceRecord } from "./use-projects";
import type { TerminalRecord } from "./use-terminals";

export interface ProjectItemProps {
  tree: ProjectTree;
  /** Terminals grouped by their `workspace_id` (only this project's keys used). */
  terminalsByWorkspace: Map<string, TerminalRecord[]>;
  activeId: string | null;
  ptyIds?: Map<string, number | null>;
  /** Initial expanded state of the project (defaults to open). */
  defaultOpen?: boolean;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  /** Launch a new terminal in `workspace` (cwd = workspace.path). */
  onNewTerminal: (workspace: WorkspaceRecord) => void;
  /** Add a workspace to THIS project (opens the folder picker / name flow). */
  onAddWorkspace: (tree: ProjectTree) => void;
  /** Open the edit (rename) modal for THIS project. */
  onEditProject?: (tree: ProjectTree) => void;
  /** Open the delete-confirm modal for THIS project. */
  onDeleteProject?: (tree: ProjectTree) => void;
  /** Open the "Manage commands" modal for THIS project (PRD-3 command templates). */
  onManageCommands?: (tree: ProjectTree) => void;
  /** Commands (PRD-3 instances) grouped by `workspace_id`, for this project's keys. */
  commandsByWorkspace?: Map<string, CommandRecord[]>;
  /** Record id of the active command row (drives the shared selection rail). */
  activeCommandId?: string | null;
  /** Select a command row (mounts its view in the main pane). */
  onSelectCommand?: (id: string) => void;
  /** Persist a new order for a workspace's terminals (within-workspace only). */
  onReorderTerminals?: (workspaceId: string, ids: string[]) => void;
  /**
   * Persist THIS project's open/closed band state so it survives a restart.
   * Called on every toggle with the new `collapsed` value (open → collapsed=true).
   */
  onSetCollapsed?: (id: string, collapsed: boolean) => void;
  /** Persist a workspace band's open/closed state (threaded to `<WorkspaceItem>`). */
  onSetWorkspaceCollapsed?: (id: string, collapsed: boolean) => void;
}

/**
 * `<ProjectItem>` — one project in the sidebar spine, re-aligned to the v6
 * prototype (finding 01KV35FYP877FQ2B7970BTBXXN): a FULL-BLEED band header
 * (`bg-sidebar-accent`, hairline-divided, sticky) carrying a chevron, a small
 * project IDENTITY dot (`--primary`), the project NAME once, a quiet
 * mono-font SUMMARY shown only when collapsed ("N ws" for a multi-workspace
 * project, "N term" for a mono-root), and a hover-revealed kebab menu (⋮) bundling
 * the project actions (Rename / Add workspace / Delete).
 *
 *  - single root workspace → the body folds straight into the root's typed
 *    subsections (NO "main" row — stays shallow), per `showWorkspaceSection`;
 *  - multiple workspaces → the body is one `<WorkspaceItem>` per workspace, the
 *    ROOT relabeled `"main"` and the others by their distinguishing names.
 */
export function ProjectItem({
  tree,
  terminalsByWorkspace,
  activeId,
  ptyIds,
  defaultOpen = true,
  onSelect,
  onClose,
  onNewTerminal,
  onAddWorkspace,
  onEditProject,
  onDeleteProject,
  onManageCommands,
  commandsByWorkspace,
  activeCommandId,
  onSelectCommand,
  onReorderTerminals,
  onSetCollapsed,
  onSetWorkspaceCollapsed,
}: ProjectItemProps) {
  // Initialize the band's open state from the PERSISTED `collapsed` flag so the
  // disclosure is restored on reload (open = !collapsed). `defaultOpen` is the
  // fallback when the record carries no flag (isolation tests pass a bare tree).
  const [open, setOpen] = useState(
    tree.project.collapsed != null ? !tree.project.collapsed : defaultOpen,
  );
  const reduced = useReducedMotion();
  const { project, workspaces } = tree;
  const sectioned = showWorkspaceSection(workspaces);

  // Toggle the band AND persist the new disclosure (open → collapsed=true), so it
  // is restored on the next launch. The optimistic local flip keeps the UI
  // instant; `onSetCollapsed` writes through to the backend.
  const toggleOpen = () => {
    setOpen((v) => {
      const next = !v;
      onSetCollapsed?.(project.id, !next);
      return next;
    });
  };

  const termsFor = (wsId: string) => terminalsByWorkspace.get(wsId) ?? [];
  const cmdsFor = (wsId: string) => commandsByWorkspace?.get(wsId) ?? [];

  // Collapsed-band summary: a multi-workspace project shows "N ws"; a mono-root
  // shows its terminal count "N term" (proto's quiet mono summary, finding F).
  const termCount = workspaces.reduce(
    (n, ws) => n + (terminalsByWorkspace.get(ws.id)?.length ?? 0),
    0,
  );
  const summary = sectioned
    ? `${workspaces.length} ws`
    : `${termCount} term${termCount === 1 ? "" : "s"}`;

  return (
    // NO `layout` prop: the rows now animate a REAL height collapse (see
    // `item-motion.ts`), so the band's own size follows the closing/opening row
    // continuously in NORMAL DOCUMENT FLOW and sibling bands reflow on their own.
    // A `layout` projection here would be a SECOND animator on top of that flow —
    // exactly the double-tp we removed. (Shared rail FLIP still works: it rides
    // the `layoutId` element + the surrounding `LayoutGroup`, not this band.)
    <motion.li className="flex flex-col">
      {/* FULL-BLEED band header (proto's `.pband`): flat fill, hairline divider.
          The toggle and the kebab are SIBLINGS (a button cannot nest a button),
          so the toggle button itself carries `aria-expanded`. */}
      <div className="group flex items-center border-b border-sidebar-border bg-sidebar-accent/60 hover:bg-sidebar-accent">
        <button
          type="button"
          aria-expanded={open}
          onClick={toggleOpen}
          className="flex min-w-0 flex-1 items-center gap-2 px-3 py-2 text-left text-sm font-semibold text-sidebar-foreground select-none"
        >
          <motion.span
            aria-hidden
            animate={{ rotate: open ? 90 : 0 }}
            transition={itemTransition(reduced)}
            className="flex shrink-0 items-center text-muted-foreground"
          >
            <ChevronRightIcon className="size-3.5" />
          </motion.span>
          {/* Project identity dot (proto's `.pdot` — magenta `--primary`). */}
          <span aria-hidden className="size-1.5 shrink-0 rounded-full bg-primary" />
          <span className="min-w-0 flex-1 truncate">{project.name}</span>
          {!open && (
            <span className="shrink-0 font-mono text-xs font-normal text-muted-foreground/60 tabular-nums">
              {summary}
            </span>
          )}
        </button>

        {/* One hover-revealed kebab menu bundles the project actions. Hidden when
            the band is collapsed (proto: collapsed kebab opacity 0). */}
        <div
          className={cn(
            "shrink-0 pr-2 opacity-0 transition focus-within:opacity-100 data-popup-open:opacity-100",
            open && "group-hover:opacity-100",
          )}
        >
          <Menu
            tooltip="Project actions"
            trigger={
              <Button
                variant="ghost"
                size="icon-xs"
                aria-label={`Project actions for ${project.name}`}
                className="size-5"
              >
                <MoreVerticalIcon />
              </Button>
            }
          >
            {onEditProject && (
              <MenuItem
                icon={<PencilIcon className="size-4" />}
                onClick={() => onEditProject(tree)}
                aria-label={`Rename project ${project.name}`}
              >
                Rename
              </MenuItem>
            )}
            <MenuItem
              icon={<FolderPlusIcon className="size-4" />}
              onClick={() => onAddWorkspace(tree)}
              aria-label={`Add workspace to ${project.name}`}
            >
              Add workspace
            </MenuItem>
            {onManageCommands && (
              <MenuItem
                icon={<TerminalSquareIcon className="size-4" />}
                onClick={() => onManageCommands(tree)}
                aria-label={`Manage commands for ${project.name}`}
              >
                Manage commands
              </MenuItem>
            )}
            {onDeleteProject && (
              <>
                <MenuSeparator />
                <MenuItem
                  destructive
                  icon={<Trash2Icon className="size-4" />}
                  onClick={() => onDeleteProject(tree)}
                  aria-label={`Delete project ${project.name}`}
                >
                  Delete
                </MenuItem>
              </>
            )}
          </Menu>
        </div>
      </div>

      <CollapsibleSection open={open}>
        {sectioned ? (
          // Multi-workspace: one WorkspaceItem per workspace, minimal indent.
          <ul className="flex flex-col gap-0.5 px-2.5 pt-1 pb-1">
            {workspaces.map((ws) => (
              <WorkspaceItem
                key={ws.id}
                workspace={ws}
                displayLabel={workspaceDisplayLabel(ws, project.name)}
                terminals={termsFor(ws.id)}
                commands={cmdsFor(ws.id)}
                activeId={activeId}
                activeCommandId={activeCommandId}
                ptyIds={ptyIds}
                showHeader
                onSelect={onSelect}
                onClose={onClose}
                onNewTerminal={onNewTerminal}
                onSelectCommand={onSelectCommand}
                onManageCommands={onManageCommands ? () => onManageCommands(tree) : undefined}
                onReorderTerminals={onReorderTerminals}
                onSetCollapsed={onSetWorkspaceCollapsed}
              />
            ))}
          </ul>
        ) : (
          // Mono-root: no workspace section; fold straight into the root's
          // subsections. The root always exists (create_project makes it).
          workspaces[0] && (
            <div className="px-2.5 pt-1 pb-1">
              <WorkspaceItem
                workspace={workspaces[0]}
                terminals={termsFor(workspaces[0].id)}
                commands={cmdsFor(workspaces[0].id)}
                activeId={activeId}
                activeCommandId={activeCommandId}
                ptyIds={ptyIds}
                showHeader={false}
                onSelect={onSelect}
                onClose={onClose}
                onNewTerminal={onNewTerminal}
                onSelectCommand={onSelectCommand}
                onManageCommands={onManageCommands ? () => onManageCommands(tree) : undefined}
                onReorderTerminals={onReorderTerminals}
              />
            </div>
          )
        )}
      </CollapsibleSection>
    </motion.li>
  );
}
