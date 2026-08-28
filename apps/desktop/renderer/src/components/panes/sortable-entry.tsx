import type { ReactNode } from "react";
import { useSortable, type UseSortableInput } from "@dnd-kit/react/sortable";
import { OptimisticSortingPlugin } from "@dnd-kit/dom/sortable";
import { move } from "@dnd-kit/helpers";

import { cn } from "@/lib/utils";
import { dragPreview, mergeTarget, projectDrop, slotOf, type DragSlots } from "./group-drag";

/**
 * THE DND WIRING OF A TWO-LEVEL GROUP LIST — shared by the terminals' sidebar and the commands
 * modal, so the gesture is configured ONCE (review 01M11WE6H9E5D5FCS6RT0ZXAXN).
 *
 * `reorder-dnd.ts` states in writing that no surface may "reintroduce a competing drag config".
 * That is about the SENSORS, and it is not the only thing that could drift: the collision
 * priorities, the plugin set and the way the insertion trait is anchored are just as much part of
 * "how a drop behaves", and a second copy of them in the commands modal would have diverged from
 * the sidebar's the first time either was tuned. They live here, with the pure {@link dragPreview}
 * they are the rendering of.
 */

/**
 * dnd-kit's default sortable plugins MINUS {@link OptimisticSortingPlugin} — the one thing that
 * would move the list under the pointer, which is the exact opposite of what these lists promise.
 *
 * That plugin previews a re-order by REPARENTING the DOM: it calls
 * `target.insertAdjacentElement("beforebegin" | "afterend", source)`, so the dragged element becomes
 * a SIBLING of the target — i.e. a child of the TARGET'S positioning wrapper. Two things break at
 * the first step of every drag: (1) the wrapper ↔ item pairing this file is built on (the trait is
 * an overlay inside the wrapper, the item is the wrapper's only other child) stops holding, so the
 * trait ends up drawn against an item that is no longer there; and (2) React still owns those
 * nodes, so unmounting a list dnd-kit has reparented throws `HierarchyRequestError`. The optimistic
 * move ALSO animates every displaced sibling (`defaultSortableTransition`, 250ms), which is the
 * measurable "the rows moved under the pointer" the drop-indicator walk forbids.
 *
 * Dropping it costs nothing: the projection is already computed on every `dragover`
 * ({@link dragPreview} over `move(slots, event)`) and PAINTED as the insertion trait. The preview
 * is the trait, not a live rehearsal of the list.
 *
 * Hoisted to module scope on purpose — `useSortable` compares this option by identity/deep-equal on
 * every render, and a fresh closure would re-register the plugin set each time.
 */
type SortablePlugins = Extract<NonNullable<UseSortableInput["plugins"]>, readonly unknown[]>;

const withoutOptimisticSorting = (defaults: SortablePlugins): SortablePlugins =>
  defaults.filter((plugin) => plugin !== OptimisticSortingPlugin);

/**
 * THE NESTING IS RESOLVED BY PRIORITY, not by pixels — a block's droppable CONTAINS its members'.
 *
 * dnd-kit sorts collisions by priority FIRST and only then by value (`sortCollisions`), and every
 * entry used to sit at the default `Normal`. A pointer inside a member row therefore collided with
 * the row AND with the block enclosing it as equals, and the winner was whichever the value happened
 * to favour that frame. Walking a row up through a block, the answer flipped to the block at each
 * seam between two members: the trait left the block and came back ("D goes before C" → "D leaves
 * the group" → "D goes before B"). The same tie also made an insertion BETWEEN two members
 * unreachable — on a two-member block the projection jumped straight from "after B" to "before A".
 *
 * Blocks therefore collide at `Low` and rows at `Normal`: A MEMBER ROW UNDER THE POINTER ALWAYS
 * WINS, and the block only takes the drop where it is the only thing there — its head, its padding,
 * the gap under its last member — which is exactly "the row lands BESIDE the block, not inside it".
 * The three in-block positions stay reachable, from the side you approach: coming down from above
 * gives "first" then "second", coming up from below gives "last" then "second", each monotonically.
 *
 * `CollisionPriority` is not re-exported by `@dnd-kit/dom`, so the two levels are spelled as the
 * numbers that enum gives them.
 */
const BLOCK_COLLISION_PRIORITY = 1; // CollisionPriority.Low
const ROW_COLLISION_PRIORITY = 2; // CollisionPriority.Normal

/** What a two-level list PAINTS while a row (or a block) is held. */
export interface LivePreview {
  /** The entry the trait is drawn against, or `null` when there is nothing to point at. */
  readonly anchorId: string | null;
  /** `top` = the row falls BEFORE the anchor; `bottom` = after it. */
  readonly edge: "top" | "bottom";
  /** The block the drop lands inside and that the floor allows — it lights up. */
  readonly armedGroupId: string | null;
  /** The block the drop lands inside but the floor refuses — it never lights up at the same time. */
  readonly refusedGroupId: string | null;
  /** The two-member block about to lose one and vanish with it. */
  readonly dissolvingGroupId: string | null;
  /**
   * THE ROOT ROW THE DROP WOULD MAKE A FOLDER WITH (gesture 5) — it takes the ring, and the
   * insertion trait is not drawn at all: the two readings are exclusive, so the release is never
   * ambiguous. `null` whenever the pointer is on one of the target's EDGE bands, which is a plain
   * re-order and keeps the trait.
   */
  readonly mergeWithId: string | null;
  /** Same row, but the 180×120px floor forbids the birth — never set alongside `mergeWithId`. */
  readonly mergeRefusedId: string | null;
}

/**
 * Whether two readings are the SAME pendant — the guard that keeps a per-frame recomputation from
 * being a per-frame RE-RENDER.
 *
 * The band of gesture 5 is a position INSIDE a line, so the preview has to be recomputed on every
 * `dragmove` and not only when the drop target changes (`dragover` fires on a target CHANGE alone,
 * which is why a band read once at a line's top edge never updated). Recomputing is cheap; telling
 * React the list changed sixty times a second is not, and it is also the thing the whole
 * `withoutOptimisticSorting` decision exists to avoid.
 */
export function samePreview(a: LivePreview | null, b: LivePreview | null): boolean {
  if (a === b) return true;
  if (a == null || b == null) return false;
  return (
    a.anchorId === b.anchorId &&
    a.edge === b.edge &&
    a.armedGroupId === b.armedGroupId &&
    a.refusedGroupId === b.refusedGroupId &&
    a.dissolvingGroupId === b.dissolvingGroupId &&
    a.mergeWithId === b.mergeWithId &&
    a.mergeRefusedId === b.mergeRefusedId
  );
}

/**
 * RE-READ THE BAND ALONE, on a `dragmove` — the half that has to be measured EVERY frame.
 *
 * ## Why this is not just `readDragPreview` again (and must never be)
 *
 * `dragover` fires only when the drop TARGET CHANGES, so a POSITION INSIDE a line — which is exactly
 * what gesture 5's band is — is read once, at the edge the pointer entered through, and then never
 * again. The ring would arm or not arm depending on which side of the line you came in from.
 *
 * But `readDragPreview` runs `move()`, and `@dnd-kit/helpers`' `move` calls `event.preventDefault()`
 * on several ordinary paths (no target yet, the projected index already equals the current one…).
 * On a `dragover` that only refuses a drop target; on a `dragmove` it FREEZES THE DRAG — dnd-kit
 * skips `position.current = coordinates` when the event was prevented, so the held row stops
 * following the pointer and every indicator goes with it. Measured: wiring `readDragPreview` to
 * `onDragMove` made all four existing gestures unreachable.
 *
 * So the frame-by-frame reading is the BAND and nothing else. The projection — which slot, which
 * anchor, which block is armed — stays the one the last `dragover` computed, which is correct
 * because it can only change when the target does.
 */
export function withMergeBand(
  current: LivePreview,
  slots: DragSlots,
  event: DragEvent,
  canCreate?: () => boolean,
): LivePreview {
  const merging = readMergeTarget(slots, event);
  const allowed = merging == null || (canCreate?.() ?? true);
  const next: LivePreview = {
    ...current,
    mergeWithId: allowed ? merging : null,
    mergeRefusedId: allowed ? null : merging,
  };
  return samePreview(current, next) ? current : next;
}

/** The dragged entry's id, or `null` for an event without a source (a cancel). */
export function sourceId(event: {
  operation: { source: { id: string | number } | null };
}): string | null {
  const id = event.operation.source?.id;
  return id == null ? null : String(id);
}

/**
 * The shape of the dnd-kit event both readings below need — the target, and where the drag IS.
 *
 * `to` is what a `dragmove` carries (the coordinates the move is going to); `operation.position` is
 * the live one and is what a `dragover` has. Reading `to` first is not a preference: `dragmove`
 * snapshots the operation BEFORE applying the move, so its `position` is one frame stale and the
 * band would lag the pointer by a step.
 */
type DragEvent = Parameters<typeof move>[1] & {
  to?: { x: number; y: number };
  operation: {
    source: { id: string | number } | null;
    target?: { id: string | number; element?: Element | null } | null;
    position?: { current?: { x: number; y: number } };
  };
};

/**
 * THE ROW THE DROP WOULD MERGE WITH, measured — the geometry half of gesture 5.
 *
 * `mergeTarget` decides everything that can be decided from the model; the one thing it cannot know
 * is WHERE IN THE TARGET LINE the drag currently is, and that needs the DOM. `operation.position` is
 * the drag's live point in client coordinates for BOTH sensors — the pointer's own coordinates while
 * dragging with a mouse, the dragged element's centre stepped by the arrow keys otherwise — so the
 * band is reachable without a pointer, which is the condition every gesture in this list is held to.
 *
 * Exported because the COMMIT must ask the very same question the pendant asked: the list calls it
 * again in `onDragEnd` and hands the answer to `dropIntent`, so what lands is what the ring promised.
 */
export function readMergeTarget(slots: DragSlots, event: DragEvent): string | null {
  const draggedId = sourceId(event);
  const target = event.operation.target;
  const point = event.to ?? event.operation.position?.current;
  if (draggedId == null || target?.element == null || point == null) return null;
  const box = target.element.getBoundingClientRect();
  if (!(box.height > 0)) return null;
  return mergeTarget(slots, draggedId, String(target.id), (point.y - box.top) / box.height);
}

/**
 * Read a `dragover` as the indicators the list must paint. Pure but for `canInsert` / `canCreate`,
 * which are the questions this module cannot answer itself — only the tiling host can be measured.
 */
export function readDragPreview(
  slots: DragSlots,
  event: DragEvent,
  canInsert: (groupId: string, index: number) => boolean,
  isFolded?: (groupId: string) => boolean,
  /** Whether the floor allows a group to be BORN. Absent ⇒ this surface never refuses a birth. */
  canCreate?: () => boolean,
): LivePreview | null {
  const draggedId = sourceId(event);
  if (draggedId == null) return null;
  // The SAME projection the drop will commit — including the folded-block case `move()` cannot see.
  const after = projectDrop(slots, event, isFolded);
  const projected = dragPreview(slots, after, draggedId);
  if (projected == null) return null;

  // The trait sits BEFORE `beforeId`, or AFTER the last sibling that is not the row in the air.
  const siblings = after[projected.parent] ?? [];
  const last = [...siblings].reverse().find((id) => id !== draggedId) ?? null;
  const anchorId = projected.beforeId ?? last;
  const edge = projected.beforeId != null ? "top" : "bottom";

  // ENTERING is the only case the floor can refuse: re-ordering inside a block adds no pane.
  const entering = projected.groupId != null && slotOf(slots, draggedId) !== projected.parent;
  const refusedGroupId =
    entering && projected.groupId != null && !canInsert(projected.groupId, projected.index)
      ? projected.groupId
      : null;
  // The BAND is layered on the fresh projection here too, so a `dragover` and the `dragmove` that
  // follows it can never disagree about which of the two readings is live.
  return withMergeBand(
    {
      anchorId,
      edge,
      armedGroupId: refusedGroupId == null ? projected.groupId : null,
      refusedGroupId,
      dissolvingGroupId: projected.dissolvingGroupId,
      mergeWithId: null,
      mergeRefusedId: null,
    },
    slots,
    event,
    canCreate,
  );
}

/**
 * ONE ENTRY OF A SLOT — its positioning wrapper, its sortable registration, and the insertion trait
 * when the drop is aimed at it.
 *
 * The wrapper is what makes the trait an OVERLAY rather than a row in the flow: it is the
 * positioning block, the item (a `[data-terminal-row]`, a `[data-command-row]` or a
 * `[data-group-block]`) is its only child in the layout, and the trait is absolutely placed against
 * its top or bottom edge. Nothing moves under the pointer while the drop is being aimed — which is
 * true only because {@link withoutOptimisticSorting} takes dnd-kit's optimistic re-order out of the
 * plugin set.
 */
export function SortableEntry({
  entryId,
  slot,
  index,
  preview,
  kind,
  children,
}: {
  /** The id this entry has IN ITS SLOT — a row id, or a block's `block:<groupId>` slot id. */
  entryId: string;
  slot: string;
  index: number;
  preview: LivePreview | null;
  kind: "row" | "block";
  children: (handles: {
    ref: (element: HTMLElement | null) => void;
    handleRef: (element: HTMLElement | null) => void;
    dragging: boolean;
  }) => ReactNode;
}) {
  const sortable = useSortable({
    id: entryId,
    index,
    group: slot,
    type: kind,
    // A row lands next to a row and INSIDE a block; a block only ever sorts among root entries.
    // Accepting both on every entry keeps that a question of WHERE the drop resolves (which
    // `dropIntent` answers) rather than of what dnd-kit was willing to look at.
    accept: ["row", "block"],
    plugins: withoutOptimisticSorting,
    collisionPriority: kind === "block" ? BLOCK_COLLISION_PRIORITY : ROW_COLLISION_PRIORITY,
  });
  // THE TWO READINGS ARE EXCLUSIVE — a release must never be able to mean two things. While a line
  // is being merged with, NO trait is drawn anywhere: the projection underneath still says where the
  // row would have fallen (it is the last `dragover`'s, and only a target change can move it), but
  // the band overrides it and the ring is the only thing on screen.
  const merged = preview != null && (preview.mergeWithId != null || preview.mergeRefusedId != null);
  const trait = !merged && preview?.anchorId === entryId ? preview.edge : null;
  // GESTURE 5's PENDANT, on the LINE ITSELF — the same ring a block wears when a drop will land
  // inside it, and the same destructive outline it wears when the floor refuses, because it is the
  // same family of answer: "these will be one folder". Drawn on the positioning WRAPPER so both
  // families get it from one place and neither row component has to know the gesture exists.
  const merging = preview?.mergeWithId === entryId;
  const mergeRefused = preview?.mergeRefusedId === entryId;
  return (
    <div
      data-drop-merge={merging || undefined}
      data-drop-merge-refused={mergeRefused || undefined}
      className={cn(
        "relative",
        // A RING, never a border: arming a line must not resize it by a pixel, or the list moves
        // under the pointer at the exact moment it must not.
        (merging || mergeRefused) && "rounded-md ring-1",
        merging && "bg-ring/10 ring-ring",
        mergeRefused && "ring-destructive",
      )}
    >
      {trait != null ? (
        <div
          data-drop-insertion
          data-drop-insertion-edge={trait}
          // WHERE THE ROW WILL FALL, drawn in the 2px gutter between two items — a real, opaque
          // surface, above everything so nothing can cover it (human review
          // 01M0CY3F075ACP4TFBK1KJ49X8: an attribute in the DOM is not a trait on the screen).
          className={cn(
            "pointer-events-none absolute inset-x-0 z-20 h-0.5 rounded-full bg-primary",
            trait === "top" ? "-top-0.5" : "-bottom-0.5",
          )}
        />
      ) : null}
      {children({
        ref: sortable.ref as (element: HTMLElement | null) => void,
        handleRef: sortable.handleRef as (element: HTMLElement | null) => void,
        dragging: sortable.isDragging === true,
      })}
    </div>
  );
}
