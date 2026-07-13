import type { ReactNode } from "react";
import { DragDropProvider } from "@dnd-kit/react";
import { useSortable } from "@dnd-kit/react/sortable";
import { move } from "@dnd-kit/helpers";

import { cn } from "@/lib/utils";
import { sortByOrder, type ReorderGroup } from "@/components/sidebar/reorder-utils";
import { EmptyHint } from "./tree-primitives";
import { reorderSensors, sameOrder } from "./reorder-dnd";
import { useTerminalReorder } from "./use-terminal-reorder";

/**
 * `<SortableTerminalRows>` — the v2.5.6 cockpit's drag-sortable terminal list, shared by the
 * workspace runtime tree's Terminals section AND the loose-terminals band. It is the ACTIVE-shell
 * rebranch of v1's `sortable-terminal-list.tsx`: the SAME dnd-kit setup (one `DragDropProvider`
 * per list so a row never drags into another group, the shared pointer+keyboard `reorderSensors`,
 * `move(ids, event)` to compute the new order, and a commit ONLY on `onDragEnd`).
 *
 * Reorder is scoped to ONE group (a workspace's terminals, or the loose section) — never across
 * groups — and the drop is persisted through the reorder CONTEXT (`useTerminalReorder`), which
 * splices the group order into the global sequence and calls the backend `reorder` (the order
 * PERSISTS in each terminal's `order_index`). The rows are rendered in the OPTIMISTIC global
 * order (`sortByOrder` against `orderedIds`) so a drop repaints at once without waiting on a
 * snapshot re-pull.
 *
 * Whole-row drag with the 5px pointer activation (like v1's terminal rows): a plain click still
 * SELECTS the row and the hover rename/close controls still fire (they `stopPropagation`), so the
 * drag never breaks the phase-3 affordances. The row content is handed in via `renderRow` to keep
 * this list decoupled from `<TerminalRow>` (no import cycle with the runtime tree).
 */
export function SortableTerminalRows<T extends { id: string }>({
  terminals,
  group,
  empty,
  renderRow,
}: {
  /** The group's terminals (a workspace's rows, or the loose ones). */
  terminals: readonly T[];
  /** The reorder group: a workspace id, or `null` for the loose section. */
  group: ReorderGroup;
  /** Muted hint shown when the group is empty. */
  empty: string;
  /** Render one terminal's row (e.g. `<TerminalRow terminal={t} activity={…} />`). */
  renderRow: (terminal: T) => ReactNode;
}) {
  const { reorderTerminals, orderedIds } = useTerminalReorder();
  // Render in the live global order so a drop stays put (the pure `reorder` DB command does not
  // re-push the snapshot). Idempotent for the loose band, which is already in that order.
  const sorted = sortByOrder(terminals, orderedIds, (t) => t.id);
  const ids = sorted.map((t) => t.id);

  if (sorted.length === 0) {
    return <EmptyHint>{empty}</EmptyHint>;
  }

  return (
    <DragDropProvider
      sensors={reorderSensors}
      onDragEnd={(event) => {
        if (event.canceled) return;
        const next = move(ids, event);
        if (!sameOrder(next, ids)) reorderTerminals(group, next);
      }}
    >
      <div className="flex flex-col gap-0.5 px-2">
        {sorted.map((terminal, index) => (
          <SortableTerminalRow key={terminal.id} id={terminal.id} index={index}>
            {renderRow(terminal)}
          </SortableTerminalRow>
        ))}
      </div>
    </DragDropProvider>
  );
}

/**
 * One drag-sortable terminal row: a PLAIN wrapper `<div>` dnd-kit owns (`useSortable`'s `ref`)
 * around the handed-in row content. dnd-kit drives this element's transform during a drag/reflow;
 * the inner `<TerminalRow>` keeps its own click/rename/close behaviour untouched.
 */
function SortableTerminalRow({
  id,
  index,
  children,
}: {
  id: string;
  index: number;
  children: ReactNode;
}) {
  const { ref, isDragging } = useSortable({ id, index });
  return (
    <div
      ref={ref}
      data-terminal-row
      data-dragging={isDragging || undefined}
      className={cn(isDragging && "opacity-60")}
    >
      {children}
    </div>
  );
}

export default SortableTerminalRows;
