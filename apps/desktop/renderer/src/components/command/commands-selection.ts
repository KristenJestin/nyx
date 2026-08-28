import type { CommandGroup } from "./use-command-groups";

/**
 * WHAT THE COMMANDS SCREEN IS LOOKING AT — the one piece of state the master/detail turns on
 * (finding 01M132XFZ5KY8QMJ79HZZSBV3J).
 *
 * The screen used to be a stack of surfaces that each appeared and disappeared: a create form that
 * pushed the list down, an edit form that opened INSIDE a card, an import TAB that replaced the
 * list, and a grouping section below the fold. Four placements for four things that are all "what
 * am I editing right now" — so they are one value, and the catalogue on the left never moves.
 *
 * It is a closed union rather than four booleans on purpose: two of them could never be true at
 * once and a boolean pair says they could.
 */
export type CommandsSelection =
  /** Nothing chosen — the panel shows what the catalogue IS, and how to compose it. */
  | { readonly kind: "none" }
  /** One command template: its fields, its position, its package.json provenance. */
  | { readonly kind: "command"; readonly id: string }
  /** One folder: its name, its colour, its tiling shape, and its dissolution. */
  | { readonly kind: "group"; readonly id: string }
  /** The package.json import — AN ENTRY OF THE LIST, never a tab: the catalogue stays visible. */
  | { readonly kind: "import" }
  /** A command being created. Same placement as an edit, which is the point. */
  | { readonly kind: "new" };

/** The resting selection — nothing viewed. */
export const NO_SELECTION: CommandsSelection = { kind: "none" };

/** Is `commandId` the viewed command? */
export function isCommandSelected(selection: CommandsSelection, commandId: string): boolean {
  return selection.kind === "command" && selection.id === commandId;
}

/** Is `groupId` the viewed folder? */
export function isGroupSelected(selection: CommandsSelection, groupId: string): boolean {
  return selection.kind === "group" && selection.id === groupId;
}

/**
 * WHERE A COMMAND RUNS — the sentence the detail panel puts under a command's name.
 *
 * A command is either a member of a folder, at a RANK that is also its pane number (the ordered
 * member list IS the disposition — `use-command-groups.ts`), or it is on its own and takes the
 * whole frame. `null` for the second case, so the caller writes the "alone" sentence and this
 * function never invents words.
 */
export function commandPanePosition(
  group: CommandGroup | null,
  commandId: string,
): { readonly group: CommandGroup; readonly paneNumber: number } | null {
  if (group == null) return null;
  const index = group.command_ids.indexOf(commandId);
  return index < 0 ? null : { group, paneNumber: index + 1 };
}
