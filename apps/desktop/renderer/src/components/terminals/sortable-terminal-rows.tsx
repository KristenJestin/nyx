import { useRef, useState, type ReactNode } from "react";
import { DragDropProvider } from "@dnd-kit/react";

import { cn } from "@/lib/utils";
import { sortByOrder, type ReorderGroup } from "@/components/sidebar/reorder-utils";
import { EmptyHint } from "@/components/ui/tree-primitives";
import { GroupBlock } from "@/components/panes/group-block";
import { groupListBlocks, type ListBlock } from "@/components/panes/group-list-blocks";
import {
  blockSlotId,
  buildDragSlots,
  dropIntent,
  flattenDragSlots,
  projectDrop,
  ROOT_SLOT,
} from "@/components/panes/group-drag";
import {
  readDragPreview,
  withMergeBand,
  SortableEntry,
  sourceId,
  type LivePreview,
} from "@/components/panes/sortable-entry";
import { reorderSensors, sameOrder } from "@/components/panes/reorder-dnd";
import { useGroupFolds } from "@/components/panes/use-group-folds";
import { aggregateState, type RunState } from "@/components/ui/status-dot";
import { loadGroupOpen, saveGroupOpen } from "./ui-persistence";
import { useTerminalGroups } from "./use-terminal-groups";
import { useTerminalReorder } from "./use-terminal-reorder";

/**
 * `<SortableTerminalRows>` — the cockpit's drag-sortable terminal list, shared by the workspace
 * runtime tree's Terminals section AND (historically) the loose-terminals band. It is at once the
 * v2.5.6 reorder surface and, since this PRD, THE PLACE THE GROUPS ARE DRAWN AND COMPOSED.
 *
 * ## ONE LIST, WITH BLOCKS IN IT
 *
 * A terminal that joins a group does NOT change section — it changes appearance (the user's rule,
 * review 01M0ABR6V501D0SPR2BYBVZC4A). {@link groupListBlocks} is what keeps that true: it pulls a
 * group's members contiguous at the position of the group's FIRST member and moves nothing else,
 * and this component paints a {@link GroupBlock} around each such run. Every other row stays bare.
 * The membership itself is READ from `useTerminalGroups`, which republishes `usePaneLayout`'s
 * collection — there is no second copy of it here.
 *
 * ## THE LIST IS TWO-LEVEL, AND THAT IS WHAT MAKES THE FOUR GESTURES ONE CODE PATH
 *
 * dnd-kit sorts a RECORD (`{ [slot]: ids }`), not a flat array: {@link ROOT_SLOT} holds the bare
 * rows and the blocks, and each block holds its own members under {@link blockSlotId}. A drop is
 * then read ONCE, by `group-drag.ts`'s pure {@link dropIntent}, and it answers which of the four
 * gestures happened — re-order inside a block, leave a block, enter one, or move a whole block.
 * Nothing is decided here: this file wires dnd-kit to that verdict and commits it.
 *
 * A block's SORTABLE ID IS ITS SLOT KEY on purpose. `move()` resolves a drop onto a container by
 * looking the target id up as a record key, so `block:g1` being both the draggable's id and the
 * children slot is what lets a row land INSIDE the block it was dropped on.
 *
 * ## THE "PENDANT" CARRIES THE DESIGN
 *
 * The four gestures are told apart by the pointer's position alone, so what is painted WHILE the
 * row is held has to be unambiguous — and it is computed from the same projection the drop will
 * commit ({@link dragPreview} over `move(slots, event)`), never from a second guess:
 *  - the target block is ARMED (a ring) when the drop lands inside it;
 *  - it is REFUSED instead (never both) when the 180×120px floor forbids the addition — asked of
 *    `canInsertIntoGroup`, the only side that can measure the tiling host;
 *  - a two-member block about to lose one is shown DISSOLVING;
 *  - an INSERTION TRAIT shows WHERE the row falls: an overlay inside the target item's own
 *    positioning wrapper, on its `top` edge ("before it") or its `bottom` edge ("after it"). It is
 *    an overlay and not a row in the flow so the list does not jump under the pointer, and it is
 *    inside the block's column when the drop lands in a block (human review
 *    01M0CY3F075ACP4TFBK1KJ49X8: « il m'affiche pas ou ca va apparaitre »).
 *
 * Whole-row drag with the 5px pointer activation: a plain click still SELECTS the row and the hover
 * rename/close controls still fire (they `stopPropagation`). A BLOCK is grabbed by its whole HEAD
 * (`handleRef`, human review 01M1312FSV6MAR0P63SEG2RF6C) — one level up from the member rows, so a
 * member and its block are never picked up by the same press — and the same 5px activation is what
 * lets that head also be the fold TOGGLE, with no arbitrage to write: a press that never moved is a
 * click, and the click folds (`group-block.tsx`). The row content is handed in via `renderRow` to
 * keep this list decoupled from `<TerminalRow>` (no import cycle with the runtime tree).
 */
export function SortableTerminalRows<T extends { id: string }>({
  terminals,
  group,
  empty,
  renderRow,
  memberState,
}: {
  /** The group's terminals (a workspace's rows, or the loose ones). */
  terminals: readonly T[];
  /** The reorder group: a workspace id, or `null` for the loose section. */
  group: ReorderGroup;
  /** Muted hint shown when the group is empty. */
  empty: string;
  /** Render one terminal's row (e.g. `<TerminalRow terminal={t} activity={…} />`). */
  renderRow: (terminal: T) => ReactNode;
  /**
   * ONE terminal's run state, for a CLOSED folder's rollup dot. `renderRow` is opaque to this list
   * — it hands back a node, not a state — so the caller that KNOWS the projection supplies it here
   * (`workspace-runtime-tree.tsx`, from the very `terminalRollupState` the workspace accordion
   * aggregates). Absent ⇒ a folder carries no dot, which is what a harness with no runtime wants.
   */
  memberState?: (terminal: T) => RunState;
}) {
  const { reorderTerminals, orderedIds } = useTerminalReorder();
  const groups = useTerminalGroups();
  // What is painted WHILE the pointer is down. `null` outside a drag.
  //
  // MIRRORED IN A REF, and that is the whole "no surprise at the release" rule made structural: the
  // drop reads THE PENDANT THAT WAS ON SCREEN rather than re-deriving the band from the dragend
  // event. Re-deriving is what allows a commit to disagree with what the user was looking at — the
  // pointer's position at the release and the last frame the list rendered are not the same instant,
  // and at the keyboard they are reliably one step apart.
  const [preview, setPreview] = useState<LivePreview | null>(null);
  const previewRef = useRef<LivePreview | null>(null);
  const showPreview = (next: LivePreview | null) => {
    previewRef.current = next;
    setPreview(next);
  };
  // WHICH BLOCKS ARE FOLDED — held for the whole list, not per block, because the DRAG needs to read
  // it: a folded block has no member rows for dnd-kit to collide with, and `projectDrop` is what
  // keeps it a valid drop target anyway. Terminal groups default OPEN (a live workspace you look at)
  // and remember every later choice in the sidebar's own registry.
  const folds = useGroupFolds({ defaultOpen: true, load: loadGroupOpen, save: saveGroupOpen });
  const isFolded = (groupId: string) => !folds.isOpen(groupId);

  // Render in the live global order so a drop stays put (the pure `reorder` DB command does not
  // re-push the snapshot). Idempotent for the loose band, which is already in that order.
  const sorted = sortByOrder(terminals, orderedIds, (t) => t.id);
  const blocks = groupListBlocks(sorted, (t) => t.id, groups.groupOf);
  const slots = buildDragSlots(blocks, (t) => t.id);
  const ids = flattenDragSlots(slots);

  if (sorted.length === 0) {
    return <EmptyHint>{empty}</EmptyHint>;
  }

  /**
   * WHETHER A GROUP MAY BE BORN AT ALL on this surface, asked while the row is still in the air.
   *
   * Two conditions, and both are refusals the user must SEE rather than discover after the release:
   * the surface has to offer the gesture (`createGroupWith` is `null` on a list mounted without the
   * provider — a component test, the legacy sidebar), and the 180×120px floor has to allow a second
   * pane in this host. A refused birth wears the destructive outline on the target line, exactly as
   * a refused insertion wears it on a block.
   */
  const canCreate = () => groups.createGroupWith != null && groups.canCreateGroup();

  /** The list order the drop produced — persisted only when it really changed. */
  const commitOrder = (next: readonly string[]) => {
    if (!sameOrder(next, ids)) reorderTerminals(group, [...next]);
  };

  return (
    <DragDropProvider
      sensors={reorderSensors}
      // TWO HANDLERS, TWO DIFFERENT READINGS, and the split is load-bearing (see `withMergeBand`).
      // `dragover` fires when the drop TARGET changes and recomputes the whole projection; `dragmove`
      // fires every frame and re-reads the BAND ALONE — a position inside a line has no other way of
      // being noticed, and running the projection here would freeze the drag outright.
      onDragMove={(event) => {
        const current = previewRef.current;
        if (current != null) showPreview(withMergeBand(current, slots, event, canCreate));
      }}
      onDragOver={(event) =>
        showPreview(readDragPreview(slots, event, groups.canInsertIntoGroup, isFolded, canCreate))
      }
      onDragEnd={(event) => {
        // THE PENDANT THAT WAS ON SCREEN is the verdict — read before it is cleared.
        const shown = previewRef.current;
        showPreview(null);
        if (event.canceled) return;
        const draggedId = sourceId(event);
        if (draggedId == null) return;
        // A REFUSED merge is still a MERGE, and is handed to the intent as one: `createGroupWith`
        // owns the refusal and announces it. Dropping the id here would silently turn a refused
        // birth into a plain re-order — a different gesture than the one the line was showing.
        const merge = shown?.mergeWithId ?? shown?.mergeRefusedId ?? null;
        const intent = dropIntent(slots, projectDrop(slots, event, isFolded), draggedId, merge);
        switch (intent.kind) {
          case "none":
            return;
          case "reorder-list":
            commitOrder(intent.ids);
            return;
          case "reorder-members":
            // Membership is untouched — only the DISPOSITION of the group's panes changes, and the
            // list order follows it so the sidebar and the tiling read the same sequence.
            groups.reorderGroupMembers(intent.groupId, intent.memberIds);
            commitOrder(intent.ids);
            return;
          case "leave":
            groups.leaveGroup(intent.terminalId);
            commitOrder(intent.ids);
            return;
          case "enter":
            // The floor is asked ONE last time at the drop: the host may have been resized while
            // the row was in the air, and a refused addition must leave the model untouched.
            if (!groups.canInsertIntoGroup(intent.groupId, intent.index)) return;
            groups.insertIntoGroup(intent.groupId, intent.terminalId, intent.index);
            commitOrder(intent.ids);
            return;
          case "create":
            // THE FIFTH GESTURE, mounted here at last (human review 01M1313GN2ESW0BCM8S1G0ZY2P).
            // « si je veux créer un groupe avec les terminaux déjà existant … comment ça se
            // passe ? » — it could not be done: a terminal group was born from a SPLIT and from
            // nothing else, so two terminals that were already there had no gesture to join them.
            //
            // NOTHING IS DECIDED HERE: `dropIntent` names BOTH members (the held row and the line it
            // was dropped on) and composes the list order the pair has to end up in. The floor is
            // not re-asked either — `createGroupWith` owns that question and announces its own
            // refusal, exactly as `movePane` does; asking twice would risk two answers.
            if (groups.createGroupWith == null) return;
            groups.createGroupWith(intent.terminalId, intent.partnerId);
            commitOrder(intent.ids);
            return;
        }
      }}
    >
      <div className="flex flex-col gap-0.5 px-2">
        {blocks.map((block, index) =>
          block.kind === "solo" ? (
            <SortableEntry
              key={block.item.id}
              entryId={block.item.id}
              slot={ROOT_SLOT}
              index={index}
              preview={preview}
              kind="row"
            >
              {({ ref, dragging }) => (
                <TerminalRowShell ref_={ref} dragging={dragging}>
                  {renderRow(block.item)}
                </TerminalRowShell>
              )}
            </SortableEntry>
          ) : (
            <SortableEntry
              key={block.group.id}
              entryId={blockSlotId(block.group.id)}
              slot={ROOT_SLOT}
              index={index}
              preview={preview}
              kind="block"
            >
              {({ ref, handleRef, dragging }) => (
                <TerminalGroupBlock
                  block={block}
                  preview={preview}
                  renderRow={renderRow}
                  memberState={memberState}
                  containerRef={ref}
                  handleRef={handleRef}
                  dragging={dragging}
                  open={folds.isOpen(block.group.id)}
                  onToggle={() => folds.toggle(block.group.id)}
                  onOpenPanel={() => groups.openPanel(block.group.id)}
                  // THE FOLDER LINE ACTIVATES ITS GROUP (human review 01M14112THEB4094B2G614SXR5).
                  // `showGroup` is `usePaneLayout`'s own, threaded through the groups context, so
                  // the sidebar invents no navigation of its own; `null` on a surface with no tiling
                  // leaves the head folding on click, exactly as before.
                  onSelect={
                    groups.showGroup == null ? undefined : () => groups.showGroup?.(block.group.id)
                  }
                  // SELECTED = "this group is what the frame is showing", which is a fact the tiling
                  // already publishes. It is INDEPENDENT of the fold: a folded folder whose group is
                  // on screen wears its selected fill AND its rollup dot at once.
                  selected={
                    groups.showGroup == null ? undefined : groups.shownGroupId === block.group.id
                  }
                />
              )}
            </SortableEntry>
          ),
        )}
      </div>
    </DragDropProvider>
  );
}

/**
 * ONE TERMINAL GROUP — the shared {@link GroupBlock}, plus the ROLLUP its closed folder carries.
 *
 * THE ROLLUP IS NOT OPTIONAL DECORATION. Folding hides the members' dots, so an agent waiting on an
 * answer inside a closed folder would go invisible — which is the same reason the workspace
 * accordion has one. It is `aggregateState` over the members' own line states, i.e. the exact
 * aggregate that accordion computes, so the two dots can never disagree about the same terminals.
 *
 * The fold state itself is NOT held here: the drag has to read it (a folded block has no member rows
 * for dnd-kit to collide with — see `projectDrop`), so the whole list owns it and hands it down.
 */
function TerminalGroupBlock<T extends { id: string }>({
  block,
  preview,
  renderRow,
  memberState,
  containerRef,
  handleRef,
  dragging,
  open,
  onToggle,
  onOpenPanel,
  onSelect,
  selected,
}: {
  block: Extract<ListBlock<T>, { kind: "group" }>;
  preview: LivePreview | null;
  renderRow: (terminal: T) => ReactNode;
  memberState?: (terminal: T) => RunState;
  containerRef: (element: HTMLElement | null) => void;
  handleRef: (element: HTMLElement | null) => void;
  dragging: boolean;
  open: boolean;
  onToggle: () => void;
  onOpenPanel: () => void;
  onSelect?: () => void;
  selected?: boolean;
}) {
  const rollup =
    memberState == null ? null : aggregateState(block.items.map((item) => memberState(item)));
  return (
    <GroupBlock
      groupId={block.group.id}
      color={block.group.color}
      name={block.group.name}
      containerRef={containerRef}
      handleRef={handleRef}
      dragging={dragging}
      open={open}
      onToggle={onToggle}
      onSelect={onSelect}
      selected={selected}
      rollup={rollup ?? undefined}
      armed={preview?.armedGroupId === block.group.id}
      refused={preview?.refusedGroupId === block.group.id}
      dissolving={preview?.dissolvingGroupId === block.group.id}
      onOpenPanel={onOpenPanel}
    >
      <BlockMembers
        block={block}
        preview={preview}
        renderRow={renderRow}
        slot={blockSlotId(block.group.id)}
      />
    </GroupBlock>
  );
}

/** The member rows of one block — their own sortable slot, so a drop inside it is gesture 1. */
function BlockMembers<T extends { id: string }>({
  block,
  preview,
  renderRow,
  slot,
}: {
  block: Extract<ListBlock<T>, { kind: "group" }>;
  preview: LivePreview | null;
  renderRow: (terminal: T) => ReactNode;
  slot: string;
}) {
  return (
    <div className="flex flex-col gap-0.5">
      {block.items.map((member, index) => (
        <SortableEntry
          key={member.id}
          entryId={member.id}
          slot={slot}
          index={index}
          preview={preview}
          kind="row"
        >
          {({ ref, dragging }) => (
            <TerminalRowShell ref_={ref} dragging={dragging}>
              {renderRow(member)}
            </TerminalRowShell>
          )}
        </SortableEntry>
      ))}
    </div>
  );
}

/**
 * One drag-sortable terminal row: a PLAIN wrapper `<div>` dnd-kit owns (`useSortable`'s `ref`)
 * around the handed-in row content. dnd-kit drives this element's transform during a drag/reflow;
 * the inner `<TerminalRow>` keeps its own click/rename/close behaviour untouched.
 *
 * THE LIFTED ROW IS OPAQUE (human review 01M0CY3F075ACP4TFBK1KJ49X8). It used to fade to 60 %,
 * which drew it and whatever it flew over in the same place — read on screen as a second, offset
 * copy of the row. It keeps its full opacity and paints its OWN surface instead, so the thing in
 * the hand is one object.
 */
function TerminalRowShell({
  ref_,
  dragging,
  children,
}: {
  ref_: (element: HTMLElement | null) => void;
  dragging: boolean;
  children: ReactNode;
}) {
  return (
    <div
      ref={ref_}
      data-terminal-row
      data-dragging={dragging || undefined}
      className={cn(dragging && "z-10 rounded-md bg-background shadow-sm")}
    >
      {children}
    </div>
  );
}

export default SortableTerminalRows;
