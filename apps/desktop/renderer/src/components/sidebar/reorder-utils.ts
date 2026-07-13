import type { TerminalRecord } from "./use-terminals";

/**
 * The grouping a reorder is scoped to: a specific workspace by id, or the LOOSE
 * group (unattached terminals, `workspace_id == null`). Drag-reorder is scoped to
 * ONE group at a time — within a workspace's Terminaux, or within the top-level
 * loose TERMINALS section — never across groups.
 */
export type ReorderGroup = string | null;

/** Whether a terminal belongs to the given reorder group. */
function inGroup(t: TerminalRecord, group: ReorderGroup): boolean {
  // Normalise an absent/empty `workspace_id` to the loose group (null).
  const ws = t.workspace_id ?? null;
  return ws === group;
}

/**
 * Splice an in-GROUP reorder into the GLOBAL terminal id sequence.
 *
 * Drag-reorder is scoped to ONE group: either a single workspace's Terminaux
 * (01KV2V46…) or the loose TERMINALS section (01KV2V4AWT…, `workspace_id ==
 * null`). The backend `reorder(ids)` sets each terminal's `order_index` to its
 * position in the FULL list, so persisting only the dragged group's slice would
 * recompute every other terminal's order and corrupt cross-group ordering.
 * Instead we rebuild the WHOLE sequence: every terminal keeps its slot, except
 * the dragged group's terminals, whose slots are filled in the new `orderedIds`
 * order (in place). Pure → unit-testable.
 *
 * @param terminals  the full terminal list, in current global order.
 * @param group      the group whose terminals were reordered (workspace id, or
 *                   `null` for the loose section).
 * @param orderedIds that group's terminal ids in the new (dragged) order.
 * @returns the full id sequence to persist via `reorder`.
 */
export function spliceWorkspaceOrder(
  terminals: TerminalRecord[],
  group: ReorderGroup,
  orderedIds: string[],
): string[] {
  // The ids of terminals ACTUALLY in this group right now. We pull the new order
  // only from these, so a stale/foreign id in `orderedIds` (e.g. a row closed or
  // auto-attached out between drag-start and drop) cannot be spliced into a live
  // slot and shift a real terminal out of the sequence — the FIFO stays aligned
  // with the in-group slots it fills.
  const groupIds = new Set(terminals.filter((t) => inGroup(t, group)).map((t) => t.id));
  const inThisGroup = new Set(orderedIds);
  const queue = orderedIds.filter((id) => groupIds.has(id));
  const next: string[] = [];
  for (const t of terminals) {
    if (inGroup(t, group) && inThisGroup.has(t.id)) {
      // Fall back to the terminal's own id on an (impossible-by-construction)
      // queue underflow so a slot is never dropped.
      next.push(queue.shift() ?? t.id);
    } else {
      next.push(t.id);
    }
  }
  return next;
}

/**
 * Sort `items` to follow the id sequence `order`, keeping any item whose id is NOT in `order`
 * in its original relative position at the END. Pure + stable → unit-testable.
 *
 * This is the v2.5.6 cockpit's LIVE-order overlay for the workspace runtime tree: the terminal
 * rows render off the `workspace_runtime_snapshot` (which the pure sidebar `reorder` DB command
 * does NOT re-push — `db_command.rs`), so after a drag we can't wait for a snapshot re-pull to
 * reflect the new order. Instead we sort the snapshot's terminals by the OPTIMISTIC global order
 * `useTerminals` already maintains (its `reorder` folds the new `order_index` in immediately) —
 * so the tree repaints in the dragged order at once, and a later re-pull (which returns the same
 * persisted `order_index` order) is a no-op, never a snap-back. Robust to membership drift: an
 * id present in `order` but absent from `items` is skipped; an item absent from `order` (a fresh
 * terminal, or a pre-load empty order) keeps its place rather than being dropped.
 *
 * @param items  the rows to order (a workspace's snapshot terminals, or the loose ones).
 * @param order  the authoritative global id order (`useTerminals().terminals` ids).
 * @param idOf   how to read an item's id.
 */
export function sortByOrder<T>(
  items: readonly T[],
  order: readonly string[],
  idOf: (item: T) => string,
): T[] {
  const rank = new Map(order.map((id, index) => [id, index]));
  return items
    .map((item, index) => ({ item, index }))
    .sort((a, b) => {
      const ra = rank.get(idOf(a.item));
      const rb = rank.get(idOf(b.item));
      // Both unranked (absent from `order`): preserve their original relative order.
      if (ra === undefined && rb === undefined) return a.index - b.index;
      // A ranked item always precedes an unranked one (unranked sink to the end).
      if (ra === undefined) return 1;
      if (rb === undefined) return -1;
      // Both ranked: by rank, then original index as a stable tiebreak.
      return ra - rb || a.index - b.index;
    })
    .map((entry) => entry.item);
}
