import { useRef, useState } from "react";
import { DownloadIcon, PlusIcon, Trash2Icon } from "lucide-react";
import { DragDropProvider } from "@dnd-kit/react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { GroupBlock } from "@/components/panes/group-block";
import {
  SidebarLine,
  SidebarLineActions,
  SidebarLineBand,
  SidebarLineFamilyGlyph,
  SidebarLineGlyph,
  SidebarLineLabel,
} from "@/components/ui/sidebar-line";
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
import { DRAG_INERT_ATTR, reorderSensors, sameOrder } from "@/components/panes/reorder-dnd";
import { useGroupFolds } from "@/components/panes/use-group-folds";
import { MicroLabel } from "@/components/ui/tree-primitives";
import { canCreateCommandGroup, canInsertIntoCommandGroup, refuseForFloor } from "./command-floor";
import { isCommandSelected, isGroupSelected, type CommandsSelection } from "./commands-selection";
import type { CommandRow } from "./command-group-tree";
import type { CommandGroup, CommandGroupsController } from "./use-command-groups";

/**
 * THE CATALOGUE — the MASTER half of the commands screen, and the ONE list on it.
 *
 * ## The defect this replaces (finding 01M132XFZ5KY8QMJ79HZZSBV3J, maquette
 * `.shots/design/commands-modal-v1.html`)
 *
 * The same list was drawn TWICE, in two idioms, one under the other: five commands made ten rows —
 * a bordered two-line card at the top, and 400 px lower the same command again as a gripped row in
 * a "MULTI-PANE GROUPING" section, with nothing saying the two were one object. Everything else
 * followed from it: the modal overflowed, the layout editor was buried three levels deep inside
 * each block, the grouping lived below the fold, the import was promoted to a TAB that HID the
 * list while you imported into it, and the count was written in two places.
 *
 * **One list, where a group is a folder** — exactly the rule the sidebar settled (review
 * 01M0ABR6V501D0SPR2BYBVZC4A). A command that joins a group does not change SECTION, it changes
 * APPEARANCE: it keeps its actions, the group becomes a folder, and the indent says the belonging.
 * The column never re-organises itself around what is selected; creating, editing and importing all
 * happen in the panel on the right, so nothing here is ever pushed down or replaced.
 *
 * ## What it composes rather than restates
 *
 *  - `panes/group-block.tsx` on `ui/sidebar-line.tsx` — the SIDEBAR'S OWN folder, not a second one;
 *  - `panes/group-drag.ts`, `panes/reorder-dnd.ts`, `panes/sortable-entry.tsx` — the six gestures,
 *    their visual pendant (armed / refused / dissolving / insertion trait / merge ring) and the
 *    5 px activation, all unchanged. This file only had to keep CARRYING them, and it does because
 *    it draws the very same block;
 *  - `panes/pane-geometry.ts` through `command-floor.ts` — the 180×120px floor, measured on the
 *    REAL host rather than counted in members.
 *
 * ## The lines are grabbed the way the sidebar's are
 *
 * A row hands dnd-kit its ELEMENT and no handle, so the whole line is the grip — the same shape
 * `sortable-terminal-rows.tsx` uses, and the reason `reorder-dnd.ts` opts out of dnd-kit's
 * `preventActivation`. The gripped `⠿` this replaces was a 24×24 target for a gesture the whole
 * 272 px line can carry, and it stood exactly where the family glyph belongs.
 *
 * NOTHING ARBITRATES between grabbing and selecting: the sensor arms past 5 px, so a press that
 * never moved is a click and the click selects. A drag that ends where it started therefore
 * SELECTS — the sidebar's behaviour, kept as it is.
 *
 * ## And the LIFECYCLE is still not here (finding 01M11XVEFEQ524XRT9ARPQFGSV)
 *
 * No play, no stop, no restart: this screen works on TEMPLATES (project-scoped) while running a
 * command drives `command_instances` (workspace-scoped), and a project-settings modal knows no
 * workspace. The sidebar pilots, the modal composes.
 */
export function CommandCatalog({
  commands,
  controller,
  selection,
  onSelect,
  onDelete,
  importableCount,
  loading = false,
  className,
}: {
  /** Every command of the project, in list order. */
  commands: readonly CommandRow[];
  /**
   * Whether the list is still being READ.
   *
   * Distinct from `commands.length === 0`, and the distinction is a defect this fixes (review
   * finding 01M1C7EMPGY1YDXHC6F1TJPFWS): while the read was in flight the catalogue showed
   * `COMMANDS 0` and "No commands yet", i.e. it asserted an absence it had not yet checked. The
   * user read that as "there is nothing here" and was, at the time, waiting on a call that took
   * about ten seconds.
   */
  loading?: boolean;
  controller: CommandGroupsController;
  /** What the detail panel is showing — the catalogue paints it, it does not own it. */
  selection: CommandsSelection;
  onSelect: (next: CommandsSelection) => void;
  /** Delete a template from its line's trailing action. */
  onDelete: (command: CommandRow) => void;
  /**
   * How many package.json scripts are still importable, for the foot entry's count.
   * `null` ⇒ no workspace to scan, and the entry is not offered at all.
   */
  importableCount: number | null;
  className?: string;
}) {
  const { groups, groupOf, createGroup, addToGroup, removeFromGroup, setLayout, reorderCommands } =
    controller;
  // The blocks open UNFOLDED here and forget it when the modal closes: this screen is where a group
  // is COMPOSED, and you cannot drag a member into a group whose members you cannot see. The fold
  // is still held for the whole list rather than per block, because `projectDrop` needs to read it
  // — a folded block has no member rows for dnd-kit to collide with.
  const folds = useGroupFolds({ defaultOpen: true });
  const isFolded = (groupId: string) => !folds.isOpen(groupId);
  // What is painted WHILE the pointer is down. `null` outside a drag. Mirrored in a ref for the same
  // reason the sidebar mirrors it: the DROP reads the pendant that was on screen rather than
  // re-deriving the band, so a release can never commit something other than what was shown.
  const [preview, setPreview] = useState<LivePreview | null>(null);
  const previewRef = useRef<LivePreview | null>(null);
  const showPreview = (next: LivePreview | null) => {
    previewRef.current = next;
    setPreview(next);
  };
  /** The floor's two questions, bound to the live groups — measured, never counted. */
  const canInsert = (groupId: string) => canInsertIntoCommandGroup(groups, groupId);

  /**
   * The BLOCKS this screen draws — the SHARED partition (`groupListBlocks`), so the settings screen
   * and the sidebar close a block on exactly the same rule. Its members come back in LIST order and
   * are re-sorted into DISPOSITION order here: the two converge (every drop writes both), but a row
   * written before `command_reorder` existed can still hold them apart, and what this screen draws
   * has to be what the panes will show.
   */
  const blocks: ListBlock<CommandRow, CommandGroup>[] = groupListBlocks(
    commands,
    (command) => command.id,
    groupOf,
  ).map((block) =>
    block.kind === "group"
      ? {
          ...block,
          items: [...block.items].sort(
            (a, b) => block.group.command_ids.indexOf(a.id) - block.group.command_ids.indexOf(b.id),
          ),
        }
      : block,
  );

  const slots = buildDragSlots(blocks, (command) => command.id);
  const ids = flattenDragSlots(slots);

  /** The list order the drop produced — persisted only when it really changed. */
  const commitOrder = async (next: readonly string[]) => {
    if (!sameOrder(next, ids)) await reorderCommands(next);
  };

  return (
    <DragDropProvider
      sensors={reorderSensors}
      // TWO HANDLERS, TWO DIFFERENT READINGS — the same split the sidebar makes, for the same
      // reason (see `withMergeBand`): the projection follows the TARGET, the band follows the
      // POINTER, and running the projection on every frame would freeze the drag.
      onDragMove={(event) => {
        const current = previewRef.current;
        if (current != null)
          showPreview(withMergeBand(current, slots, event, canCreateCommandGroup));
      }}
      onDragOver={(event) =>
        showPreview(readDragPreview(slots, event, canInsert, isFolded, canCreateCommandGroup))
      }
      onDragEnd={(event) => {
        // THE PENDANT THAT WAS ON SCREEN is the verdict — read before it is cleared. A REFUSED merge
        // is handed on as a merge, so the refusal is announced instead of quietly becoming a
        // re-order.
        const shown = previewRef.current;
        showPreview(null);
        if (event.canceled) return;
        const dragged = sourceId(event);
        if (dragged == null) return;
        const after = projectDrop(slots, event, isFolded);
        const merge = shown?.mergeWithId ?? shown?.mergeRefusedId ?? null;
        const intent = dropIntent(slots, after, dragged, merge);
        // Every branch commits the MEMBERSHIP and then the LIST ORDER, in that order and awaited:
        // the two halves of one gesture, so a command dropped out of a group stays where it was
        // dropped instead of springing back to its creation position.
        void (async () => {
          switch (intent.kind) {
            case "none":
              return;
            case "reorder-list":
              await commitOrder(intent.ids);
              return;
            case "reorder-members":
              await setLayout(intent.groupId, intent.memberIds);
              await commitOrder(intent.ids);
              return;
            case "leave":
              await removeFromGroup(intent.terminalId);
              await commitOrder(intent.ids);
              return;
            case "enter": {
              // The floor is asked ONE last time at the drop: the host may have been resized while
              // the row was in the air, and a refused addition must leave the model untouched.
              if (!canInsert(intent.groupId)) {
                refuseForFloor();
                return;
              }
              // `command_group_add` APPENDS; the drop chose a position. The two calls are the one
              // gesture — join, then take the place the insertion trait promised.
              await addToGroup(intent.groupId, intent.terminalId);
              await setLayout(intent.groupId, after[blockSlotId(intent.groupId)] ?? []);
              await commitOrder(intent.ids);
              return;
            }
            case "create":
              // THE BIRTH NAMES BOTH OF ITS MEMBERS (human review 01M1313GN2ESW0BCM8S1G0ZY2P): the
              // command that was held, and the LINE IT WAS DROPPED ON. The dotted slot this
              // replaced had only the first and picked the second itself — dropping a command on it
              // silently took another one along, which is precisely what the review threw out.
              // `dropIntent` also composes the order the contiguous pair has to end up in.
              if (!canCreateCommandGroup()) {
                refuseForFloor();
                return;
              }
              await createGroup([intent.partnerId, intent.terminalId]);
              await commitOrder(intent.ids);
              return;
          }
        })();
      }}
    >
      <div
        data-command-catalog
        className={cn("flex min-h-0 w-68 shrink-0 flex-col border-r border-border", className)}
      >
        {/* THE HEAD — the count, written HERE and nowhere else (it used to be in the tab AND in
            the footer), and the one affordance that opens the creation panel. */}
        <div className="flex items-center gap-2 px-3 pt-3 pb-1.5">
          <MicroLabel>Commands</MicroLabel>
          {/* A COUNT, or a dash while there is nothing to count yet. `0` during the read is an
              assertion the catalogue has not earned — see the `loading` prop. */}
          <span
            data-command-count
            className="rounded-full bg-muted px-1.5 text-2xs leading-relaxed font-medium text-muted-foreground tabular-nums"
          >
            {loading && commands.length === 0 ? "—" : commands.length}
          </span>
          <Button
            type="button"
            size="xs"
            className="ml-auto"
            data-command-new
            aria-pressed={selection.kind === "new"}
            onClick={() => onSelect({ kind: "new" })}
          >
            <PlusIcon />
            New
          </Button>
        </div>

        {/* THE LIST — the only one. It scrolls on its own, so the modal keeps a stable height
            whatever the project holds and the column never moves under the pointer. */}
        <div
          data-command-list
          className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto px-2 pb-2"
        >
          {commands.length === 0 ? (
            <p
              data-command-list-status
              className="px-1 py-6 text-center text-xs text-muted-foreground/70 italic"
            >
              {loading ? "Loading commands…" : "No commands yet."}
            </p>
          ) : (
            blocks.map((block, index) =>
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
                    <CommandLine
                      command={block.item}
                      containerRef={ref}
                      dragging={dragging}
                      selected={isCommandSelected(selection, block.item.id)}
                      onSelect={() => onSelect({ kind: "command", id: block.item.id })}
                      onDelete={() => onDelete(block.item)}
                    />
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
                    <CatalogGroupBlock
                      block={block}
                      preview={preview}
                      selection={selection}
                      onSelect={onSelect}
                      onDelete={onDelete}
                      open={folds.isOpen(block.group.id)}
                      onToggle={() => folds.toggle(block.group.id)}
                      containerRef={ref}
                      handleRef={handleRef}
                      dragging={dragging}
                    />
                  )}
                </SortableEntry>
              ),
            )
          )}

          {/* NO BIRTH SLOT (human review 01M1313GN2ESW0BCM8S1G0ZY2P). The dotted "Drop a command
              here to start a group" row held permanent space for an occasional gesture, at the
              bottom of a scrolling list — sometimes out of sight at the very moment a row was being
              dragged — and it could not say who joins whom. A group is founded by dropping a line
              on the MIDDLE of another line; the ring on the target says which one, before the
              release. `sortable-entry.tsx` draws it, for this screen and the sidebar alike. */}
        </div>

        {/* THE IMPORT, AS AN ENTRY OF THE LIST — not a tab (finding 01M132XFZ5KY8QMJ79HZZSBV3J).
            A tab put a one-off source at parity with the whole catalogue AND hid the list while
            importing into it, so what landed was never seen landing. Here the list stays on screen
            and the imported commands appear in it. */}
        {importableCount == null ? null : (
          <div className="border-t border-border p-1.5">
            <Button
              type="button"
              variant="ghost"
              size="xs"
              data-command-import-entry
              aria-pressed={selection.kind === "import"}
              onClick={() => onSelect({ kind: "import" })}
              className={cn(
                "w-full justify-start text-muted-foreground",
                selection.kind === "import" && "bg-muted text-foreground",
              )}
            >
              <DownloadIcon />
              Import from package.json
              <span className="ml-auto rounded-full bg-muted px-1.5 text-2xs font-medium tabular-nums">
                {importableCount}
              </span>
            </Button>
          </div>
        )}
      </div>
    </DragDropProvider>
  );
}

/**
 * ONE FOLDER IN THE CATALOGUE — the shared {@link GroupBlock}, made SELECTABLE.
 *
 * SAME BLOCK, SAME FOLDER, DIFFERENT DEFAULT (human review 01M12MW1DV1V455EWBMV048TE6). The sidebar
 * opens a command group folded because it is a launcher; here a group is being COMPOSED, so it
 * opens UNFOLDED — you cannot drag a member into a group whose members you cannot see. And the
 * state is LOCAL rather than persisted: this is a modal whose whole state is thrown away on close.
 *
 * WHAT THE BODY NO LONGER HOLDS is the whole point of the refonte: the shape bar and the layout
 * preview used to be nested inside every block, three levels down and ~110 px per group, so a
 * project with three groups stopped being readable. They live in the folder's DETAIL PANEL now,
 * where they have room — and next to the very list whose order drives them.
 */
function CatalogGroupBlock({
  block,
  preview,
  selection,
  onSelect,
  onDelete,
  containerRef,
  handleRef,
  dragging,
  open,
  onToggle,
}: {
  block: Extract<ListBlock<CommandRow, CommandGroup>, { kind: "group" }>;
  preview: LivePreview | null;
  selection: CommandsSelection;
  onSelect: (next: CommandsSelection) => void;
  onDelete: (command: CommandRow) => void;
  containerRef: (element: HTMLElement | null) => void;
  handleRef: (element: HTMLElement | null) => void;
  dragging: boolean;
  open: boolean;
  onToggle: () => void;
}) {
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
      // THE HEAD SELECTS, THE FOLDER FOLDS — the master/detail composition of the shared block.
      // There is no `…` any more: the panel it opened held the name, the colour and "Ungroup all",
      // and all three are in the detail panel now. A second editor for one object is the very
      // duplication this screen exists to remove.
      selected={isGroupSelected(selection, block.group.id)}
      onSelect={() => onSelect({ kind: "group", id: block.group.id })}
      armed={preview?.armedGroupId === block.group.id}
      refused={preview?.refusedGroupId === block.group.id}
      dissolving={preview?.dissolvingGroupId === block.group.id}
    >
      {block.items.map((command, memberIndex) => (
        <SortableEntry
          key={command.id}
          entryId={command.id}
          slot={blockSlotId(block.group.id)}
          index={memberIndex}
          preview={preview}
          kind="row"
        >
          {({ ref: memberRef, dragging: memberDragging }) => (
            <CommandLine
              command={command}
              containerRef={memberRef}
              dragging={memberDragging}
              selected={isCommandSelected(selection, command.id)}
              onSelect={() => onSelect({ kind: "command", id: command.id })}
              onDelete={() => onDelete(command)}
            />
          )}
        </SortableEntry>
      ))}
    </GroupBlock>
  );
}

/**
 * ONE COMMAND'S LINE — the sidebar's line, wearing a command's glyph and carrying its delete.
 *
 * It is `<SidebarLine>` composed exactly as `<RuntimeRow>` composes it: a band that is a `<button>`
 * and selects, a lead glyph at the line's own size, and a trailing cluster revealed on hover /
 * selection / focus-visible by `<SidebarLineActions>`. The name is the only text — the command line
 * itself belongs to the detail panel, and a 272 px column that tried to hold both truncated each.
 *
 * THE WHOLE LINE IS THE GRIP: the sortable's ELEMENT is handed in and no handle is, which is the
 * shape `sortable-terminal-rows.tsx` uses for a terminal row. The `⠿` this replaces was a separate
 * 24×24 target standing where the family glyph goes, on a list whose every other family is grabbed
 * anywhere on its surface.
 */
function CommandLine({
  command,
  containerRef,
  dragging,
  selected,
  onSelect,
  onDelete,
}: {
  command: CommandRow;
  containerRef: (element: HTMLElement | null) => void;
  dragging: boolean;
  selected: boolean;
  onSelect: () => void;
  onDelete: () => void;
}) {
  return (
    <SidebarLine
      ref={containerRef}
      data-command-row={command.id}
      data-dragging={dragging || undefined}
      selected={selected}
      className={cn(
        // THE LIFTED ROW IS OPAQUE and paints its own surface, so the thing in the hand reads as one
        // object rather than as a second copy of the row it is flying over.
        dragging && "z-10 bg-card shadow-sm",
      )}
      actions={
        <SidebarLineActions>
          <Button
            variant="ghost-destructive"
            size="icon-xs"
            aria-label={`Delete ${command.name}`}
            // NEVER A DRAG ACTIVATOR — this button sits INSIDE the surface dnd-kit listens on (the
            // line IS the sortable's element), at the pointer and at the keyboard alike. The marker
            // is what dnd-kit itself reads (`reorder-dnd.ts`'s `preventActivation`); a React
            // `stopPropagation` could not do it, because React delegates to the root and arrives
            // after dnd-kit's own listener has already run.
            {...{ [DRAG_INERT_ATTR]: "" }}
            // …and `stopPropagation` for what React DOES own: pressing it must not select the line
            // it is about to delete.
            onPointerDown={(event) => event.stopPropagation()}
            onClick={(event) => {
              event.stopPropagation();
              onDelete();
            }}
          >
            <Trash2Icon />
          </Button>
        </SidebarLineActions>
      }
    >
      <SidebarLineBand
        render={<button type="button" onClick={onSelect} className="text-left" />}
        data-command-select={command.id}
      >
        {/* THE COMMAND FAMILY'S GLYPH, from its one source (human review 01M140VRZJG0RE5VMG91D8TH3T)
            — « dans les settings commande, j'ai pas la même icons que dans la sidebar ? ». This
            catalogue drew a bare terminal here while the sidebar drew a command; the sidebar is the
            list where a terminal, an agent and a command sit side by side and have to be told apart,
            so it is the one the glyph answers to. */}
        <SidebarLineGlyph icon={<SidebarLineFamilyGlyph family="command" />} />
        <SidebarLineLabel className="text-foreground">{command.name}</SidebarLineLabel>
      </SidebarLineBand>
    </SidebarLine>
  );
}
