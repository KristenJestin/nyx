import { createContext, useContext, useMemo, type ReactNode } from "react";

import type { ReorderGroup } from "@/components/sidebar/reorder-utils";

/**
 * DRAG-REORDER TERMINALS FROM THEIR ROW (v2.5.6, task `01KXBNX51EPQ69MVKYG4W960HD`). A terminal
 * ROW sits deep in the project → workspace → tree → row chain (and the loose-terminals band
 * renders the SAME row), so — exactly like the rename ([`use-terminal-rename`]) and close
 * ([`use-terminal-close`]) callbacks — the reorder wiring flows via CONTEXT rather than
 * prop-drilling through every intermediate component.
 *
 * It carries two things the sortable list needs:
 *  - `reorderTerminals(group, ids)` — persist a drag within ONE group (a workspace's Terminals,
 *    or the loose section) by splicing it into the GLOBAL terminal order and calling the backend
 *    `reorder` (see `useTerminalActions`). The order PERSISTS in each terminal's `order_index`.
 *  - `orderedIds` — the OPTIMISTIC global terminal order (`useTerminals().terminals` ids), the
 *    authority the tree sorts its snapshot rows by so a drop repaints in the new order at once
 *    (the pure `reorder` DB command emits no `terminals://changed`, so the snapshot itself does
 *    not re-pull — see `sortByOrder`).
 */
export interface TerminalReorder {
  /** Persist a drag within `group` (workspace id, or `null` for the loose section). */
  reorderTerminals: (group: ReorderGroup, ids: string[]) => void;
  /** The live global terminal id order the tree sorts its rows by (optimistic, reorder-aware). */
  orderedIds: readonly string[];
}

/**
 * Context carrying the reorder wiring so any terminal ROW list can drag-reorder ITSELF without
 * prop-drilling. The default is inert (a no-op persist + empty order) so a list rendered OUTSIDE
 * the provider (an isolation test) still renders and simply cannot reorder — it never throws.
 */
const TerminalReorderContext = createContext<TerminalReorder>({
  reorderTerminals: () => {},
  orderedIds: [],
});

/**
 * Provider that supplies the reorder callback + live order to the runtime terminal rows. Mount
 * once around the Terminals page tree + loose band (alongside the rename/close/stats providers).
 */
export function TerminalReorderProvider({
  reorderTerminals,
  orderedIds,
  children,
}: {
  reorderTerminals: TerminalReorder["reorderTerminals"];
  orderedIds: readonly string[];
  children: ReactNode;
}) {
  const value = useMemo<TerminalReorder>(
    () => ({ reorderTerminals, orderedIds }),
    [reorderTerminals, orderedIds],
  );
  return (
    <TerminalReorderContext.Provider value={value}>{children}</TerminalReorderContext.Provider>
  );
}

/** Read the reorder wiring from the shared context. Inert outside a provider (safe in isolation). */
export function useTerminalReorder(): TerminalReorder {
  return useContext(TerminalReorderContext);
}
