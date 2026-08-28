import { move } from "@dnd-kit/helpers";

import { sameOrder } from "./reorder-dnd";
import type { ListBlock } from "./group-list-blocks";

/**
 * THE DRAG IS HOW GROUPS ARE COMPOSED — the pure half (review 01M0BPP79090KAQ9ZAFJ23ZRTX, critical).
 *
 * ## Why this module lives in `components/panes/` and not in `components/terminals/`
 *
 * It was written for the sidebar's terminal list and sat next to it. The COMMANDS modal now runs the
 * very same two-level model (review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding 01M11XTRXVVPZ9XMDW5ARWKWHA),
 * and `components/command/` importing `components/terminals/` would re-open the import cycle finding
 * 01M0ZMHM9GSC39SCN2DBRQ8VX3 closed. `components/panes/` is the neutral module both families already
 * depend on — the block, the palette, the geometry and the list partition all live here — so the
 * gesture joins them and the dependency stays one-way: `terminals/` → `panes/`, `command/` → `panes/`,
 * and `panes/` → neither.
 *
 * ## What was wrong
 *
 * The sidebar has had a dnd-kit sorter since before this PRD, and it knew nothing about groups:
 * `sortable-terminal-rows.tsx` documented it as scoped to one workspace or to the loose section,
 * "never across groups". It computed a new ORDER and called `reorderTerminals`, while the block was
 * painted OVER an already-sorted list (`groupListBlocks` merely pulls a group's members contiguous).
 * Dropping a row inside a block, or pulling it out, therefore changed its POSITION and never its
 * MEMBERSHIP — verbatim: « je peux deposer ou enlever des terminaux comme ca et c'est jamais pris en
 * compte par ton systeme ». The most natural gesture of the list was the one that did nothing.
 *
 * ## The shape that fixes it
 *
 * The list stops being one flat sequence and becomes a TWO-LEVEL one, which is what it always looked
 * like on screen:
 *
 *  - {@link ROOT_SLOT} holds the bare rows AND the blocks, in list order;
 *  - each block holds its own members, under {@link blockSlotId}.
 *
 * dnd-kit sorts exactly this shape (`move(record, event)` moves an item ACROSS parents), so the four
 * gestures fall out of one question — where did the row end up? — rather than four code paths:
 *
 *  1. same block, new index  → RE-ORDER the members;
 *  2. block → root           → LEAVE the group;
 *  3. root → block           → ENTER it at the insertion index;
 *  4. a BLOCK moved in root  → the group travels whole, and no group is touched at all;
 *  5. row ONTO the MIDDLE of another root row → CREATE a folder of the two.
 *
 * Gesture 5 exists because two things that already exist have to be joinable. A terminal group used
 * to be born ONLY from a split — so two terminals that were already there could never be put
 * together (« si je veux créer un groupe avec les terminaux déjà existant … comment ça se passe ? »)
 * — and a COMMAND has no split at all, so its groups had no birth whatsoever.
 *
 * IT IS A BAND OF THE TARGET LINE, NOT A DROP ZONE OF ITS OWN. A root line reads as THREE bands:
 * its top {@link MERGE_BAND_RATIO}-complement edge means "before it", its bottom edge means "after
 * it", and its MIDDLE means "into a folder with it" — the rule every file manager already teaches,
 * and the very rule a block already applies (fall INSIDE it, or fall BETWEEN two of its lines). The
 * dotted "start a group" slot it replaces was worse on three measurable counts: it held permanent
 * room for an occasional gesture at the bottom of a scrolling list, it could not say WHO joins whom
 * (it had one member and picked the second itself), and with no partner available it answered
 * nothing at all. The band has none of those: the partner IS the line under the pointer, and it is
 * named by the ring drawn on it before the release.
 *
 * Everything here is PURE, and deliberately so: the interpretation of a drop is the part that has to
 * be provable, and the criteria ask for both the COMMIT and the DURING-drag indicators to be
 * asserted. The dnd-kit wiring in `sortable-terminal-rows.tsx` is left with nothing to decide.
 */

/** The list itself — the parent of the bare rows and of the blocks. */
export const ROOT_SLOT = "__list__";

/**
 * THE MIDDLE BAND of a root line — the share of its height that means "make a folder with this one"
 * rather than "land before/after it". The two edges take what is left, half each.
 *
 * 40 % is what a 32 px line can spare: ~13 px of merge between two ~10 px edges, and the edges are
 * the ones that must stay easy because they are the frequent gesture. If it proves too fine at the
 * pointer, the remedy decided with the user is to widen the middle to 50 % — never to bring back a
 * permanent drop slot.
 */
export const MERGE_BAND_RATIO = 0.4;

/** What the vertical position INSIDE a root line means. */
export type DropBand = "before" | "merge" | "after";

/** Read a fraction of a line's height (0 = its top edge, 1 = its bottom) as one of the three bands. */
export function dropBand(ratio: number): DropBand {
  const edge = (1 - MERGE_BAND_RATIO) / 2;
  if (ratio < edge) return "before";
  if (ratio > 1 - edge) return "after";
  return "merge";
}

/**
 * THE ROW A DROP WOULD MERGE WITH — gesture 5's partner, or `null` when the drop means anything else.
 *
 * The GEOMETRY is the caller's (only the DOM can say where in a line a pointer sits); everything
 * that decides whether a merge is even possible is here, so both the during-drag pendant and the
 * commit read the same rule and cannot disagree:
 *
 *  - a line never merges with ITSELF;
 *  - a BLOCK is never either end of it — a block dropped on a row is gesture 4 (the group travels
 *    whole) and a row dropped on a block is gesture 3 (it enters), and both already have their own
 *    answer and their own indicator;
 *  - the target must be a ROOT row. A MEMBER row under the pointer already means "enter that
 *    block", which is the older and more specific reading, and it wins.
 *
 * The DRAGGED row may perfectly well be a member: it then leaves its group (which dissolves if it
 * was a pair) and founds the new one — the model's own "at most one group" rule, not a second commit.
 */
export function mergeTarget(
  slots: DragSlots,
  draggedId: string,
  targetId: string | null,
  ratio: number,
): string | null {
  if (targetId == null || targetId === draggedId) return null;
  if (slotGroupId(targetId) != null || slotGroupId(draggedId) != null) return null;
  if (slotOf(slots, targetId) !== ROOT_SLOT) return null;
  return dropBand(ratio) === "merge" ? targetId : null;
}

/** Namespace for a block's sortable id, so it can never collide with a terminal id. */
const BLOCK_PREFIX = "block:";

/** The sortable id (and slot key) of a group's block. */
export function blockSlotId(groupId: string): string {
  return `${BLOCK_PREFIX}${groupId}`;
}

/** The group a block slot id designates, or `null` when the id is not one. */
export function slotGroupId(slotId: string): string | null {
  return slotId.startsWith(BLOCK_PREFIX) ? slotId.slice(BLOCK_PREFIX.length) : null;
}

/**
 * The two-level list dnd-kit sorts: {@link ROOT_SLOT} → rows and blocks; a block slot → its members.
 * A plain record (not a Map) because that is the shape `@dnd-kit/helpers`' `move` mutates.
 */
export type DragSlots = Record<string, string[]>;

/** Build the slots from the rendered blocks — the SAME partition the list draws. */
export function buildDragSlots<T>(
  blocks: readonly ListBlock<T, { readonly id: string }>[],
  idOf: (item: T) => string,
): DragSlots {
  const slots: DragSlots = { [ROOT_SLOT]: [] };
  for (const block of blocks) {
    if (block.kind === "solo") {
      slots[ROOT_SLOT].push(idOf(block.item));
      continue;
    }
    const slot = blockSlotId(block.group.id);
    slots[ROOT_SLOT].push(slot);
    slots[slot] = block.items.map(idOf);
  }
  return slots;
}

/**
 * The flat terminal-id sequence the slots represent — what the list SHOWS, top to bottom, and
 * therefore what `reorderTerminals` persists. A block contributes its members in their own order.
 */
export function flattenDragSlots(slots: DragSlots): string[] {
  return (slots[ROOT_SLOT] ?? []).flatMap((entry) => {
    const groupId = slotGroupId(entry);
    return groupId == null ? [entry] : [...(slots[entry] ?? [])];
  });
}

/**
 * WHERE A DROP LANDS — `move()`'s projection, plus the ONE case `move()` structurally cannot see.
 *
 * ## A folded block would otherwise stop being a group (human review 01M12MW1DV1V455EWBMV048TE6)
 *
 * `@dnd-kit/helpers`' `move` resolves a drop target by looking its id up among the slots' CHILDREN
 * first, and only falls back to "is it a slot KEY?" when it found none. A block is BOTH — a child
 * of {@link ROOT_SLOT} and a key of its own members — and the child lookup always wins, so a drop
 * that resolved to the BLOCK has always meant "land next to it in the list". Entering a group has
 * therefore always happened through a MEMBER ROW under the pointer: the row is a child of the
 * block's slot, so `move` reads the block as the parent. That is exactly what
 * `sortable-entry.tsx`'s collision priorities are tuned around.
 *
 * Folding removes the member rows from the DOM. With no child to collide with, a folded block could
 * only ever be a SIBLING — dropping a terminal onto a folded group would silently re-order the list
 * instead of joining the group, which is the worst kind of failure because it looks like nothing
 * happening. Measured, before this existed: walking a bare row over a folded block armed nothing and
 * drew the insertion line BESIDE it, every time.
 *
 * So a drop aimed at a FOLDED block is projected here instead: the row leaves wherever it was and
 * joins that block's slot at the END. The end, and not a chosen position, because a folded group
 * shows no position to choose — the user is saying "into this group", and nothing more. Everything
 * else, including the floor refusal, is unchanged: {@link dropIntent} reads the result exactly as it
 * reads `move`'s, so the four gestures stay one code path.
 *
 * `isFolded` is the list's own state; a surface with no folding passes nothing and gets `move`.
 *
 * ## A BLOCK NEVER LANDS INSIDE A BLOCK (human review 01M140TQC52BT95TK60DD5TPYH)
 *
 * Groups do not nest, and {@link dropIntent} says so twice over — it short-circuits a dragged block
 * to `reorder-list` and {@link mergeTarget} refuses a block at either end of a birth. `move()` knows
 * none of that: a member row under the pointer is a child of ITS block's slot, so walking a block
 * over another OPEN block's members projected the block INTO that slot. The pendant read the
 * projection and armed the target block — « l'interface me montre que je peux le drag dans un autre
 * dossier … dans la réalité il le fait pas » — while the commit read `dropIntent` and re-ordered the
 * list. Worse, the flat order that projection produces is nonsense: the nested `block:<id>` string
 * flattens as if it were a terminal id and the swallowed group's own members drop out of the list
 * entirely.
 *
 * {@link blockStaysAtRoot} is the fix AT THE PROJECTION, which is the single source both readings
 * already share: there is nothing left to keep in agreement, because the divergence no longer has a
 * place to exist. The block is put back in the root list AT THE HOST BLOCK'S OWN POSITION — before
 * it when the pointer reached its first member, after it otherwise — which is the sibling reading,
 * the only one the model has for a block over a block. The pendant then draws the insertion trait
 * beside the target block and the release performs exactly that.
 */
export function projectDrop(
  before: DragSlots,
  event: Parameters<typeof move>[1] & {
    operation: { source: { id: string | number } | null; target?: { id: string | number } | null };
  },
  isFolded?: (groupId: string) => boolean,
): DragSlots {
  const projected = move(before, event) as DragSlots;
  const sourceId = event.operation.source?.id;
  const draggedEntry = sourceId == null ? null : String(sourceId);
  // Read BEFORE the folded-block branch below, and independently of `isFolded`: a surface that does
  // not fold at all still has open blocks whose member rows `move()` can swallow a block into.
  if (draggedEntry != null && slotGroupId(draggedEntry) != null) {
    return blockStaysAtRoot(projected, draggedEntry);
  }
  if (isFolded == null) return projected;
  const targetId = event.operation.target?.id;
  if (targetId == null || sourceId == null) return projected;
  const slot = String(targetId);
  const dragged = String(sourceId);
  const groupId = slotGroupId(slot);
  // Only a ROW enters a block. A BLOCK dropped on another block is gesture 4 (the group travels
  // whole) and must never be nested inside it.
  if (groupId == null || slotGroupId(dragged) != null || !isFolded(groupId)) return projected;
  if ((before[slot] ?? []).includes(dragged)) return projected;
  const next: DragSlots = {};
  for (const [key, ids] of Object.entries(before)) next[key] = ids.filter((id) => id !== dragged);
  next[slot] = [...(next[slot] ?? []), dragged];
  return next;
}

/**
 * PUT A DRAGGED BLOCK BACK IN THE ROOT LIST — see {@link projectDrop}'s "a block never lands inside
 * a block".
 *
 * A block slot id IS the block's own root entry id (`blockSlotId`), so the slot that swallowed it
 * names its host in the root list and no lookup is needed. The position is read off the projection
 * rather than guessed: `move()` placed the block at an INDEX among the host's members, and index 0
 * is the only one that can only have been reached from above — so it means "before the host" and
 * every other one means "after it". That keeps the trait monotone as the pointer walks the block.
 */
function blockStaysAtRoot(projected: DragSlots, draggedId: string): DragSlots {
  const parent = slotOf(projected, draggedId);
  if (parent == null || parent === ROOT_SLOT) return projected;
  const memberIndex = (projected[parent] ?? []).indexOf(draggedId);
  const next: DragSlots = {};
  for (const [key, ids] of Object.entries(projected)) {
    next[key] = ids.filter((id) => id !== draggedId);
  }
  const root = [...(next[ROOT_SLOT] ?? [])];
  const hostIndex = root.indexOf(parent);
  const at = hostIndex < 0 ? root.length : memberIndex === 0 ? hostIndex : hostIndex + 1;
  root.splice(at, 0, draggedId);
  next[ROOT_SLOT] = root;
  return next;
}

/** Which slot holds `id`, or `null` when it holds nothing. */
export function slotOf(slots: DragSlots, id: string): string | null {
  for (const [parent, ids] of Object.entries(slots)) {
    if (ids.includes(id)) return parent;
  }
  return null;
}

/**
 * What a drop MEANS — one of the four gestures, or nothing at all.
 *
 * `ids` is always the new flat list order to persist: a gesture that changes membership changes the
 * ORDER too (the row is drawn where it was dropped), and the two are committed together.
 */
export type DropIntent =
  | { readonly kind: "none" }
  /** The bare list re-ordered: two solo rows swapped, or a whole BLOCK moved (gesture 4). */
  | { readonly kind: "reorder-list"; readonly ids: readonly string[] }
  /** Gesture 1 — the row stayed in its block, the members' order changed. */
  | {
      readonly kind: "reorder-members";
      readonly groupId: string;
      readonly memberIds: readonly string[];
      readonly ids: readonly string[];
    }
  /** Gesture 2 — the row was dropped OUTSIDE its block and quits the group. */
  | { readonly kind: "leave"; readonly terminalId: string; readonly ids: readonly string[] }
  /** Gesture 3 — the row was dropped INSIDE a block, at `index`. */
  | {
      readonly kind: "enter";
      readonly groupId: string;
      readonly terminalId: string;
      readonly index: number;
      readonly ids: readonly string[];
    }
  /**
   * Gesture 5 — the row was dropped on the MIDDLE of another root row, and the two are to become a
   * folder. BOTH members are named: `terminalId` is what the hand was holding, `partnerId` is the
   * line it was dropped on. Nothing is guessed — the birth slot this replaces had one member and
   * chose the other itself, which is exactly the silence the user objected to.
   */
  | {
      readonly kind: "create";
      readonly terminalId: string;
      readonly partnerId: string;
      readonly ids: readonly string[];
    };

/**
 * THE LIST ORDER A BIRTH PRODUCES — the pair, contiguous, in the order the tree will read them.
 *
 * A block is drawn at the position of its FIRST member, so the pair has to end up adjacent or the
 * folder would be drawn away from half of itself. The DROPPED-ON row keeps its place and the held
 * one lands right after it, which is also the order the pane tree is built in (the partner keeps the
 * original pane, the newcomer takes the new one) — so the sidebar and the tiling read one sequence.
 */
function mergeOrder(before: DragSlots, draggedId: string, partnerId: string): string[] {
  const ids = flattenDragSlots(before).filter((id) => id !== draggedId);
  ids.splice(Math.max(ids.indexOf(partnerId), 0) + 1, 0, draggedId);
  return ids;
}

/**
 * Read a drop: where `draggedId` was, where it landed, and therefore which of the five gestures the
 * user just performed. `before` and `after` are the slots before the drag and as dnd-kit projected
 * them (`move(slots, event)`).
 *
 * `mergeWith` is the ONE thing this module cannot work out on its own: which line the pointer was
 * over, and whether it was over its middle band. The caller measures it — through the very same
 * {@link mergeTarget} it uses to paint the ring during the drag — so what is committed is what was
 * announced.
 */
export function dropIntent(
  before: DragSlots,
  after: DragSlots,
  draggedId: string,
  mergeWith: string | null = null,
): DropIntent {
  const from = slotOf(before, draggedId);
  const to = slotOf(after, draggedId);
  if (from == null || to == null) return { kind: "none" };

  // THE MERGE IS READ FIRST, and it overrides `move()`'s projection rather than extending it: the
  // helper resolved the drop as a plain re-order next to the target (that is all a sortable can
  // mean), and the band says it was a birth instead. Everything `move()` computed about the ORDER is
  // discarded here on purpose — {@link mergeOrder} is the only sequence a birth can produce.
  if (mergeWith != null && mergeWith !== draggedId && slotGroupId(draggedId) == null) {
    return {
      kind: "create",
      terminalId: draggedId,
      partnerId: mergeWith,
      ids: mergeOrder(before, draggedId, mergeWith),
    };
  }

  const ids = flattenDragSlots(after);
  // The shared order predicate, not a second copy of it: `sameOrder` is exported next door and is
  // already what this module's own consumer uses to skip a no-op reorder. `before` is flattened
  // ONCE here — the inline version rebuilt it for the length and then again for every element.
  const sameFlatOrder = sameOrder(ids, flattenDragSlots(before));

  // A BLOCK travelling: the group moves whole, its members keep their order, and no tree is
  // touched. It is a pure list re-order — which is exactly why the pane trees come out identical.
  if (slotGroupId(draggedId) != null) {
    return sameFlatOrder ? { kind: "none" } : { kind: "reorder-list", ids };
  }

  const fromGroup = slotGroupId(from);
  const toGroup = slotGroupId(to);

  if (from === to) {
    if (toGroup == null) return sameFlatOrder ? { kind: "none" } : { kind: "reorder-list", ids };
    const memberIds = after[to] ?? [];
    const unchanged = (before[to] ?? []).every((id, i) => id === memberIds[i]);
    return unchanged
      ? { kind: "none" }
      : { kind: "reorder-members", groupId: toGroup, memberIds, ids };
  }

  // OUT of a block and into the bare list: the row leaves its group and becomes solo again. (A group
  // left with a single member dissolves — that is the model's own rule, not a special case here.)
  if (toGroup == null && fromGroup != null) {
    return { kind: "leave", terminalId: draggedId, ids };
  }

  // INTO a block: from the bare list, or straight from another block (the model detaches first).
  if (toGroup != null) {
    return {
      kind: "enter",
      groupId: toGroup,
      terminalId: draggedId,
      index: Math.max((after[to] ?? []).indexOf(draggedId), 0),
      ids,
    };
  }
  return { kind: "none" };
}

/**
 * WHAT THE USER SEES WHILE STILL HOLDING THE ROW — the part that carries the design.
 *
 * The four gestures are told apart by the pointer's position alone, so the "during" has to be
 * unambiguous:
 *  - the block LIGHTS UP when the drop will land inside it (gestures 1 and 3);
 *  - it does NOT light up when the insertion line is outside (gesture 2) — the only signal that
 *    separates "I am re-ordering" from "I am pulling this row out";
 *  - an INSERTION LINE shows where the row will fall, in both cases;
 *  - a two-member block that is about to lose one is shown DISSOLVING (orchestrator arbitrage): the
 *    dissolution is an irreversible consequence of a single gesture and must not surprise.
 *
 * The REFUSAL (the 180×120px floor) is layered on top by the caller, which is the only one that can
 * measure the host — see `canInsertIntoGroup`.
 */
export interface DragPreview {
  readonly draggedId: string;
  /** The slot the drop lands in — {@link ROOT_SLOT} or a block slot. */
  readonly parent: string;
  /** The group the drop lands INSIDE, or `null` when it lands in the bare list. */
  readonly groupId: string | null;
  /** The index the row will take within `parent`. */
  readonly index: number;
  /** The id the insertion line sits BEFORE; `null` when it sits after the last row of `parent`. */
  readonly beforeId: string | null;
  /** The block that would DISSOLVE if the drop happened now (it is down to its last pair). */
  readonly dissolvingGroupId: string | null;
}

/** Read the projected slots as the indicators the list must paint. `null` when nothing moves. */
export function dragPreview(
  before: DragSlots,
  after: DragSlots,
  draggedId: string,
): DragPreview | null {
  const from = slotOf(before, draggedId);
  const parent = slotOf(after, draggedId);
  if (from == null || parent == null) return null;
  const siblings = after[parent] ?? [];
  const index = siblings.indexOf(draggedId);
  const next = siblings.slice(index + 1).find((id) => id !== draggedId) ?? null;
  const fromGroup = slotGroupId(from);
  // A group of exactly two that is losing one of them is losing itself with it.
  const dissolving =
    fromGroup != null && parent !== from && (before[from] ?? []).length === 2 ? fromGroup : null;
  return {
    draggedId,
    parent,
    groupId: slotGroupId(parent),
    index: Math.max(index, 0),
    beforeId: next,
    dissolvingGroupId: dissolving,
  };
}
