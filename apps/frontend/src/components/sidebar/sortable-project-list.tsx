import { DragDropProvider } from "@dnd-kit/react";
import { KeyboardSensor, PointerActivationConstraints, PointerSensor } from "@dnd-kit/dom";
import { move } from "@dnd-kit/helpers";

import { SortableProjectItem } from "./sortable-project-item";
import type { CommandRecord, ProjectTree, WorkspaceRecord } from "./use-projects";
import type { TerminalRecord } from "./use-terminals";

/** Movement (px) past which a pointer gesture counts as a DRAG, not a click. */
const DRAG_ACTIVATION_DISTANCE = 5;

/**
 * Sensors: a pointer sensor with a small activation distance (a tap on the grip
 * still wouldn't fire a drag) plus the keyboard sensor for a11y. Identical to the
 * terminal list (`sortable-terminal-list.tsx`).
 */
const sensors = [
  PointerSensor.configure({
    activationConstraints: [
      new PointerActivationConstraints.Distance({ value: DRAG_ACTIVATION_DISTANCE }),
    ],
  }),
  KeyboardSensor,
];

/** Shallow equality of two id sequences (same length, same order). */
function sameOrder(a: string[], b: string[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

export interface SortableProjectListProps {
  /** Projects (with their workspaces), in current order. */
  projects: ProjectTree[];
  /** Terminals grouped by their `workspace_id` (threaded to each band). */
  terminalsByWorkspace: Map<string, TerminalRecord[]>;
  activeId: string | null;
  ptyIds?: Map<string, number | null>;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  onNewTerminal: (workspace: WorkspaceRecord) => void;
  onAddWorkspace: (tree: ProjectTree) => void;
  onEditProject?: (tree: ProjectTree) => void;
  onDeleteProject?: (tree: ProjectTree) => void;
  onDeleteWorkspace?: (workspace: WorkspaceRecord) => void;
  onManageCommands?: (tree: ProjectTree) => void;
  commandsByWorkspace?: Map<string, CommandRecord[]>;
  activeCommandId?: string | null;
  onSelectCommand?: (id: string) => void;
  onReorderTerminals?: (workspaceId: string, ids: string[]) => void;
  onSetProjectCollapsed?: (id: string, collapsed: boolean) => void;
  onSetWorkspaceCollapsed?: (id: string, collapsed: boolean) => void;
  /**
   * Persist the new PROJECT order after a drag (the dragged id sequence). Optional
   * so the sidebar still renders without reorder wiring (isolation tests).
   */
  onReorderProjects?: (ids: string[]) => void;
}

/**
 * `<SortableProjectList>` — the drag-sortable top-level PROJECTS list (FEEDBACK
 * #11). Reuses the SAME dnd-kit setup as `<SortableTerminalList>`: one
 * `DragDropProvider` per list, the pointer+keyboard sensors, `move(ids, event)` to
 * compute the new order, and a commit ONLY on `onDragEnd`. Each project is a
 * `<SortableProjectItem>` carrying a grip handle; a drop hands the new id order to
 * `onReorderProjects` (→ `useProjects.reorderProjects`).
 */
export function SortableProjectList({
  projects,
  onReorderProjects,
  ...itemProps
}: SortableProjectListProps) {
  const ids = projects.map((t) => t.project.id);

  return (
    <DragDropProvider
      sensors={sensors}
      onDragEnd={(event) => {
        if (event.canceled) return;
        const next = move(ids, event);
        if (!sameOrder(next, ids)) onReorderProjects?.(next);
      }}
    >
      <ul className="flex flex-col">
        {projects.map((tree, i) => (
          <SortableProjectItem
            key={tree.project.id}
            index={i}
            tree={tree}
            terminalsByWorkspace={itemProps.terminalsByWorkspace}
            activeId={itemProps.activeId}
            ptyIds={itemProps.ptyIds}
            onSelect={itemProps.onSelect}
            onClose={itemProps.onClose}
            onNewTerminal={itemProps.onNewTerminal}
            onAddWorkspace={itemProps.onAddWorkspace}
            onEditProject={itemProps.onEditProject}
            onDeleteProject={itemProps.onDeleteProject}
            onDeleteWorkspace={itemProps.onDeleteWorkspace}
            onManageCommands={itemProps.onManageCommands}
            commandsByWorkspace={itemProps.commandsByWorkspace}
            activeCommandId={itemProps.activeCommandId}
            onSelectCommand={itemProps.onSelectCommand}
            onReorderTerminals={itemProps.onReorderTerminals}
            onSetCollapsed={itemProps.onSetProjectCollapsed}
            onSetWorkspaceCollapsed={itemProps.onSetWorkspaceCollapsed}
          />
        ))}
      </ul>
    </DragDropProvider>
  );
}

export default SortableProjectList;
