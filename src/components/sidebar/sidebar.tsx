import { PlusIcon } from "lucide-react";
import { Reorder } from "motion/react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { ReorderTerminalItem } from "./reorder-terminal-item";
import type { TerminalRecord } from "./use-terminals";

export interface SidebarProps {
  terminals: TerminalRecord[];
  activeId: string | null;
  /**
   * Live record→PTY id map. Each item uses its PTY id to read `terminal_info`
   * for the auto label (cwd + foreground program). Optional / absent ids mean
   * the item falls back to the record's cwd basename.
   */
  ptyIds?: Map<string, number | null>;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  onCreate: () => void;
  /**
   * Persist a new sidebar order (id sequence). Called as a reorder drag moves
   * items past each other with the computed order. Optional so the sidebar still
   * renders without reorder wiring (e.g. in isolation tests).
   */
  onReorder?: (ids: string[]) => void;
  /**
   * Persist a manual rename (`null` clears it back to auto-naming). Optional so
   * the sidebar renders without rename wiring in isolation.
   */
  onRename?: (id: string, label: string | null) => void;
  className?: string;
}

/**
 * `<Sidebar>` — the left rail that lists every live terminal and owns
 * navigation. A header with a `+` to add a terminal, then one selectable item
 * per terminal (active highlighted, per-item `x` to close). This is the flat
 * list for PRD 1; project/workspace grouping is a later PRD.
 *
 * Purely presentational: all state lives in `useTerminals`; the sidebar just
 * renders it and forwards intents (`onSelect`/`onClose`/`onCreate`/`onReorder`).
 * Reorder is done with motion's `Reorder` (controlled by the `terminals` id
 * order); `onReorder` is fired with the new order as items are dragged past each
 * other and persists it.
 */
export function Sidebar({
  terminals,
  activeId,
  ptyIds,
  onSelect,
  onClose,
  onCreate,
  onReorder,
  onRename,
  className,
}: SidebarProps) {
  // `Reorder.Group` is controlled by the id order; the records are the source of
  // truth, so we drive it with their ids and forward the reordered ids to persist.
  const ids = terminals.map((t) => t.id);

  return (
    <aside
      className={cn(
        "flex h-full w-56 shrink-0 flex-col border-r border-sidebar-border bg-sidebar",
        className,
      )}
    >
      <div className="flex items-center justify-between px-2 py-2">
        <span className="text-xs font-medium text-muted-foreground uppercase">
          Terminals
        </span>
        <Button
          variant="ghost"
          size="icon-xs"
          aria-label="New terminal"
          onClick={onCreate}
        >
          <PlusIcon />
        </Button>
      </div>
      {/* motion's Reorder makes the list sortable AND owns every bit of
          positional animation: the live drag follow, the slide when a row is
          added/removed (each Reorder.Item animates its own `layout`), and the
          reorder reflow — one coherent motion, no second animator to fight.
          Chrome only — the xterm viewport (in the deck) is untouched. */}
      <Reorder.Group
        axis="y"
        as="ul"
        values={ids}
        onReorder={(next: string[]) => onReorder?.(next)}
        className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto px-2 pb-2"
      >
        {terminals.map((t, i) => (
          <ReorderTerminalItem
            key={t.id}
            record={t}
            index={i}
            active={t.id === activeId}
            ptyId={ptyIds?.get(t.id) ?? null}
            onSelect={onSelect}
            onClose={onClose}
            onRename={onRename}
          />
        ))}
      </Reorder.Group>
    </aside>
  );
}

export default Sidebar;
