import { DragDropProvider } from "@dnd-kit/react";
import { KeyboardSensor, PointerActivationConstraints, PointerSensor } from "@dnd-kit/dom";
import { move } from "@dnd-kit/helpers";
import { AnimatePresence } from "motion/react";

import { SortableTerminalItem } from "./sortable-terminal-item";
import type { TerminalRecord } from "./use-terminals";

/** Movement (px) past which a pointer gesture counts as a DRAG, not a click. */
const DRAG_ACTIVATION_DISTANCE = 5;

/**
 * Sensors: a pointer sensor with a small activation distance (a tap still selects)
 * plus the keyboard sensor for a11y. Replaces the default set.
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

export interface SortableTerminalListProps {
  /** Terminals in this list (a single workspace's rows, or the loose rows). */
  terminals: TerminalRecord[];
  /** Record id of the globally-active terminal (highlighted if it's here). */
  activeId: string | null;
  /** Live record→PTY id map for the auto label. */
  ptyIds?: Map<string, number | null>;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  /** Persist the new id order after a drag (optional for isolation tests). */
  onReorder?: (ids: string[]) => void;
  /** Classes for the inner `<ul>` (spacing). */
  className?: string;
}

/**
 * `<SortableTerminalList>` — a drag-sortable terminal list shared by the workspace
 * Terminals subsection and the loose TERMINALS footer. Bare dnd-kit (the bisected,
 * flash-free setup): plain sortable `<li>` rows, `OptimisticSortingPlugin` (default)
 * drives the live reorder, the order is committed to the backend ONLY on
 * `onDragEnd`. One `DragDropProvider` per list, so a row never drags into another
 * workspace.
 *
 * `AnimatePresence` wraps the rows so a CLOSED row runs its height-collapse exit —
 * but the Motion animation lives on an INNER element of each row, NOT the sortable
 * `<li>` (see `sortable-terminal-item.tsx`), so it never fights dnd-kit's
 * reorder/drop on the `<li>` itself.
 */
export function SortableTerminalList({
  terminals,
  activeId,
  ptyIds,
  onSelect,
  onClose,
  onReorder,
  className,
}: SortableTerminalListProps) {
  const ids = terminals.map((t) => t.id);

  return (
    <DragDropProvider
      sensors={sensors}
      onDragEnd={(event) => {
        if (event.canceled) return;
        const next = move(ids, event);
        if (!sameOrder(next, ids)) onReorder?.(next);
      }}
    >
      <ul className={className}>
        <AnimatePresence initial={false}>
          {terminals.map((t, i) => (
            <SortableTerminalItem
              key={t.id}
              record={t}
              index={i}
              active={t.id === activeId}
              ptyId={ptyIds?.get(t.id) ?? null}
              onSelect={onSelect}
              onClose={onClose}
            />
          ))}
        </AnimatePresence>
      </ul>
    </DragDropProvider>
  );
}

export default SortableTerminalList;
