import { useCallback } from "react";

import { toast } from "@/components/ui/toast";
import { CommandControlsShell, type CommandAction } from "./command-controls";
import { useCommandGroupRun, type CommandGroupTally } from "./use-command-group-run";

/**
 * A COMMAND GROUP'S OWN play / stop / restart (review 01M11WE6H9E5D5FCS6RT0ZXAXN, finding
 * 01M11XVEFEQ524XRT9ARPQFGSV) — « les boutons play pause restart doivent pouvoir s'afficher pour le
 * groupe entier en plus, pour tout faire d'un coup ».
 *
 * It is the SECOND implementation of {@link CommandControlsShell}: the same three buttons, the same
 * in-flight `pending`, the same "a rejection is surfaced, never swallowed" seam, over
 * `command_group_*` instead of `command_*` and over a group's aggregate instead of one instance's
 * state. Nothing about the gesture is decided here — {@link commandGroupTally} derives the three
 * availabilities and `nyx_core::command::run_command_group` applies the semantics, once, for the
 * hand and for the agent alike.
 */
export function CommandGroupControls({
  groupId,
  workspaceId,
  groupLabel,
  tally,
  buttonSize = "icon-xs",
}: {
  groupId: string;
  /** The workspace whose INSTANCES are driven — a group has no lifecycle without one. */
  workspaceId: string;
  /** How the group is designated in the buttons' accessible names (its name, or "group N"). */
  groupLabel: string;
  /** The aggregate, already derived — the buttons never re-derive their own availability. */
  tally: CommandGroupTally;
  buttonSize?: "icon-sm" | "icon-xs";
}) {
  const runGroup = useCommandGroupRun();
  const run = useCallback(
    (action: CommandAction) => runGroup(groupId, workspaceId, action),
    [runGroup, groupId, workspaceId],
  );

  return (
    <CommandControlsShell
      run={run}
      enabled={{ start: tally.canStart, stop: tally.canStop, relaunch: tally.canRelaunch }}
      // The names say WHAT the gesture will touch, because the three do not touch the same members:
      // a play skips whatever is already up, a stop and a restart take the whole group.
      labels={{
        start: `Start the ${tally.total - tally.running} stopped in ${groupLabel}`,
        stop: `Stop all ${tally.total} in ${groupLabel}`,
        relaunch: `Restart all ${tally.total} in ${groupLabel}`,
      }}
      // The sidebar has no inline panel to host a message, so a refused group gesture speaks through
      // the same toast every other refused command mutation uses.
      onError={(_action, message) => toast.error(message)}
      // A change in the live tally is a real transition and clears a stale failure.
      resetKey={`${tally.running}/${tally.total}`}
      buttonSize={buttonSize}
      inRow
      className="shrink-0 gap-0"
    />
  );
}
