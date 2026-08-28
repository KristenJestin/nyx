import {
  DEFAULT_COMMAND_GROUP_SHAPE,
  shapeTree,
  type CommandGroupShape,
} from "@/components/panes/pane-groups";
import type { PaneNode } from "@/components/panes/pane-tree";

/**
 * THE PANE TREE OF A COMMAND GROUP, and the row the commands screens draw — the two pure things
 * both the RENDERED panes and the SETTINGS screen need, in a module that renders nothing.
 *
 * They used to live in `command-group-section.tsx`, which meant `command-pane-host.tsx` — a runtime
 * surface with no settings in it — imported a settings SCREEN to get a tree function. The master/
 * detail refonte (finding 01M132XFZ5KY8QMJ79HZZSBV3J) split that screen into a catalogue and four
 * detail panels, so the shared derivation had to stop riding on any one of them.
 */

/** One command as the commands screens need it: an id, a name, and its command line. */
export interface CommandRow {
  readonly id: string;
  readonly name: string;
  readonly command: string;
}

/**
 * Its ordered members poured into its chosen SHAPE, by the SAME {@link shapeTree} table the
 * preview and the host both read. Deterministic ids (seeded on the position) so the tree is stable
 * across renders and a test can pin the rectangles.
 *
 * The shape defaults to `auto`, which IS the old automatic placement — so a group that never chose
 * one is laid out exactly as it was before the shapes existed.
 */
export function commandGroupTree(
  commandIds: readonly string[],
  shape: CommandGroupShape = DEFAULT_COMMAND_GROUP_SHAPE,
): PaneNode | null {
  let n = 0;
  return shapeTree(commandIds, shape, (prefix) => `${prefix}-cg-${n++}`);
}
