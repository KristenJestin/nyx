import {
  layoutFitsFloor,
  MIN_PANE_HEIGHT_PX,
  MIN_PANE_WIDTH_PX,
  type HostSize,
} from "@/components/panes/pane-geometry";
import {
  DEFAULT_COMMAND_GROUP_SHAPE,
  type CommandGroupShape,
} from "@/components/panes/pane-groups";
import { toast } from "@/components/ui/toast";
import { commandGroupTree } from "./command-group-tree";
import type { CommandGroup } from "./use-command-groups";

/**
 * THE 180×120px FLOOR, ASKED OF THE REAL HOST — the one place the commands screen refuses.
 *
 * It is a module of its own since the master/detail split (finding 01M132XFZ5KY8QMJ79HZZSBV3J):
 * the CATALOGUE asks it while a row is in the air, and the folder's DETAIL PANEL asks it of each of
 * the six shapes. Two callers, one answer — a second copy would be a second floor.
 *
 * THE REFUSAL IS A MEASUREMENT, NEVER A MEMBER COUNT. The prototype refused past four members,
 * which was a stand-in; here it is {@link layoutFitsFloor} over the resulting tree against the
 * host's own measured box, so whether a drop is refused depends on the window and on the chosen
 * shape. An UNMEASURABLE host (a settings screen opened over a project with no pane on screen)
 * refuses NOTHING — the same rule `canSplitPane` follows.
 */

/**
 * The COMMAND pane host's own measured box. Read ON DEMAND, exactly like the terminals'
 * `measurePaneHost`, and `null` when the host is not mounted.
 */
export function measureCommandHost(): HostSize | null {
  if (typeof document === "undefined") return null;
  const host =
    document.querySelector("[data-command-pane-host]") ??
    document.querySelector("[data-pane-host]");
  if (host == null) return null;
  const box = host.getBoundingClientRect();
  return box.width > 0 && box.height > 0 ? { width: box.width, height: box.height } : null;
}

/**
 * Would this member list, under this shape, leave every pane over the floor? The commands' answer to
 * `canSplitPane`: a commands group has no split, its whole tree is RE-DERIVED, so the check is over
 * the resulting layout — through {@link layoutFitsFloor}, in the SAME shared geometry module.
 *
 * `host` is a parameter so a caller judging SIX shapes measures once: the six must be judged against
 * the same box, and a read per thumbnail would also be six layout flushes per render.
 */
export function commandLayoutFits(
  memberIds: readonly string[],
  shape: CommandGroupShape,
  host: HostSize | null = measureCommandHost(),
): boolean {
  const tree = commandGroupTree(memberIds, shape);
  return tree == null || layoutFitsFloor(tree, host);
}

/**
 * Could `groupId` take one more pane? The floor depends on the member COUNT and the SHAPE, never on
 * WHICH command it is, so the candidate is a placeholder — which is also what lets the answer be
 * given while the row is still in the air.
 */
export function canInsertIntoCommandGroup(
  groups: readonly CommandGroup[],
  groupId: string,
): boolean {
  const target = groups.find((group) => group.id === groupId);
  if (target == null) return true;
  return commandLayoutFits([...target.command_ids, "__candidate__"], target.layout_shape);
}

/**
 * Could a group be BORN here? The same question one member short: a fresh group is two panes in the
 * default shape, so the floor is asked of exactly that. Asked while the row is in the air, so a
 * refused birth is shown refused on the target line before the release.
 */
export function canCreateCommandGroup(): boolean {
  return commandLayoutFits(["__candidate-a__", "__candidate-b__"], DEFAULT_COMMAND_GROUP_SHAPE);
}

/** A refusal is never silent: the user aimed at a group and must be told why nothing happened. */
export function refuseForFloor(): void {
  toast.error(
    `This group cannot hold another command here — every pane needs at least ${MIN_PANE_WIDTH_PX}×${MIN_PANE_HEIGHT_PX}px.`,
  );
}
