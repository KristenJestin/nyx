import { useEffect, useRef, useState } from "react";
import { XIcon } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { resolveDisplayName, useAutoLabel } from "./auto-label";
import { itemTransition, itemVariants } from "./item-motion";
import type { TerminalRecord } from "./use-terminals";

/**
 * The human label for a terminal item, given a record + its list index, using
 * only the record (no live auto label). Kept as a thin wrapper over
 * `resolveDisplayName` for the static title (chrome bar) and existing tests:
 * manual `label` → cwd basename → `Terminal <n>`. The live, auto-named label
 * (cwd + foreground program) is resolved inside `<TerminalItemBody>` via
 * `useAutoLabel`; this static form is the no-auto fallback.
 */
export function displayName(record: TerminalRecord, index: number): string {
  return resolveDisplayName(record, index, null);
}

export interface TerminalItemProps {
  record: TerminalRecord;
  index: number;
  active: boolean;
  /**
   * Live PTY id for this terminal (or null if not spawned yet). Drives the auto
   * label (cwd + foreground program) read from `terminal_info`.
   */
  ptyId?: number | null;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  /** Persist a manual rename (`null` clears it back to auto-naming). */
  onRename?: (id: string, label: string | null) => void;
}

/**
 * The className for a sidebar terminal row. Shared by the standalone
 * `<TerminalItem>` (a `motion.li`) and the sortable `<ReorderTerminalItem>` (a
 * `Reorder.Item`) so both rows look identical. `dragging` dims the row while it
 * is being dragged.
 */
export function terminalRowClassName(active: boolean, dragging = false): string {
  return cn(
    // `select-none`: dragging a row by its grip must never text-select the
    // names (the rename input opts back into selection with `select-text`).
    "group flex items-center gap-1 overflow-hidden rounded-md px-2 py-1.5 text-sm select-none",
    active
      ? "bg-sidebar-accent text-sidebar-accent-foreground"
      : "text-sidebar-foreground hover:bg-sidebar-accent/50",
    dragging && "opacity-60",
  );
}

/**
 * The INNER content of a terminal row — drag-handle slot, the name (with inline
 * rename), and the hover-revealed close (`x`) — WITHOUT the row element itself.
 * Wrapper-agnostic so it can live inside either a plain `motion.li`
 * (`<TerminalItem>`, used in isolation tests) or a `Reorder.Item`
 * (`<ReorderTerminalItem>`, used by the live sidebar).
 *
 * The displayed name is AUTO-COMPUTED (cwd basename + foreground program, live
 * from `terminal_info`) unless the user set a MANUAL label, which always wins
 * and persists. Double-click the name to rename inline; Enter commits, Escape
 * cancels, an empty value clears back to auto-naming. The close button stops
 * propagation so clicking `x` closes the terminal without also selecting it.
 */
export function TerminalItemBody({
  record,
  index,
  active,
  ptyId = null,
  onSelect,
  onClose,
  onRename,
  dragHandle,
}: TerminalItemProps & {
  /** Optional drag-handle slot (the sortable wrapper injects the grip here). */
  dragHandle?: React.ReactNode;
}) {
  // Live auto label (debounced via the backend cache + a fixed poll). Manual
  // label takes precedence inside resolveDisplayName below.
  const auto = useAutoLabel(ptyId);
  const name = resolveDisplayName(record, index, auto);

  // Inline rename state. `editing` swaps the button for a text input.
  const [editing, setEditing] = useState(false);
  const inputRef = useRef<HTMLInputElement | null>(null);
  // Once a rename is committed OR cancelled, suppress the trailing `onBlur` that
  // fires as the input unmounts so we never double-persist or resurrect a
  // cancelled edit.
  const settledRef = useRef(false);
  useEffect(() => {
    if (editing) {
      settledRef.current = false;
      inputRef.current?.focus();
      inputRef.current?.select();
    }
  }, [editing]);

  const commit = (raw: string) => {
    if (settledRef.current) return;
    settledRef.current = true;
    setEditing(false);
    if (!onRename) return;
    const trimmed = raw.trim();
    // Empty → clear back to auto-naming; otherwise persist the manual override.
    onRename(record.id, trimmed === "" ? null : trimmed);
  };

  const cancel = () => {
    settledRef.current = true;
    setEditing(false);
  };

  return (
    <>
      {dragHandle}
      {editing ? (
        <input
          ref={inputRef}
          type="text"
          aria-label={`Rename terminal ${name}`}
          defaultValue={record.label ?? name}
          onClick={(e) => e.stopPropagation()}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit((e.target as HTMLInputElement).value);
            else if (e.key === "Escape") cancel();
          }}
          onBlur={(e) => commit(e.target.value)}
          className="min-w-0 flex-1 rounded-sm bg-background px-1 text-sm text-foreground outline-none ring-1 ring-sidebar-ring select-text"
        />
      ) : (
        <button
          type="button"
          onClick={() => onSelect(record.id)}
          onDoubleClick={() => onRename && setEditing(true)}
          className="min-w-0 flex-1 cursor-pointer truncate text-left outline-none"
        >
          {name}
        </button>
      )}
      <Button
        variant="ghost-destructive"
        size="icon-xs"
        aria-label={`Close terminal ${name}`}
        onClick={(e) => {
          e.stopPropagation();
          onClose(record.id);
        }}
        className={cn(
          "size-5 shrink-0 rounded opacity-0 transition focus-visible:opacity-100 group-hover:opacity-100",
          active && "opacity-100",
        )}
      >
        <XIcon className="size-3.5" />
      </Button>
    </>
  );
}

/**
 * Standalone presentational row: a `motion.li` hosting `<TerminalItemBody>`.
 * Used in isolation tests (and anywhere a row is rendered outside a Reorder
 * context); the live sidebar uses `<ReorderTerminalItem>` instead, which makes
 * the row drag-sortable. The row fades IN on mount (opacity); all positional
 * motion is owned by Reorder in the sortable variant (see item-motion).
 *
 * Animation is chrome-only and never touches the xterm viewport (a hard rule).
 */
export function TerminalItem(props: TerminalItemProps) {
  const reduced = useReducedMotion();
  return (
    <motion.li
      variants={itemVariants}
      initial="initial"
      animate="animate"
      transition={itemTransition(reduced)}
      aria-current={props.active ? "true" : undefined}
      className={terminalRowClassName(props.active)}
    >
      <TerminalItemBody {...props} />
    </motion.li>
  );
}

export default TerminalItem;
