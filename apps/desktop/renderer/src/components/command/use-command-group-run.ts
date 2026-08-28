import { useCallback } from "react";

import { nyxBridge } from "@/bridge";
import type { ExecState } from "@/components/sidebar/use-terminals";
import type { CommandAction } from "./command-controls";

/**
 * THE GROUP-WIDE LIFECYCLE — play / stop / restart on every member of a command group at once.
 *
 * ## Why it lives HERE and not in `use-command-groups.ts` (review 01M11WE6H9E5D5FCS6RT0ZXAXN,
 * finding 01M11XVEFEQ524XRT9ARPQFGSV)
 *
 * `useCommandGroups` is PROJECT-scoped: it reads and edits `managed_command_groups`, which hold
 * command TEMPLATES. Running one is `command_instances`, which are per WORKSPACE. The two never mix,
 * and that split is the whole reason these controls sit in the SIDEBAR and not in the project
 * settings modal: the modal knows no workspace, so it structurally cannot pilot anything. The
 * sidebar pilots, the modal composes. Keeping the three routes in their own module says so in the
 * file tree instead of in a comment.
 *
 * ## The RULE is not written here
 *
 * "Play starts only the members that are not started, stop stops everyone, restart re-spawns the
 * active AND starts the inactive" is `nyx_core::command::run_command_group`, and it is written ONCE:
 * the MCP tools (`start_command` / `stop_command` / `relaunch_command` given a `group_id` +
 * `workspace_id`) call the very same function. If this hook looped over the members itself, the rule
 * would exist twice and could diverge. It sends ONE call per gesture and reads back the plan the
 * backend executed.
 *
 * The three routes are spelled out one by one rather than looked up in a table, for the same reason
 * the sidecar keeps three dispatch arms: `command-group-ipc-contract.test.ts` DERIVES the renderer's
 * half of the wire contract by reading each route name and the object literal that follows it
 * straight out of this source, and a lookup table would hide the argument names from it.
 */

/** One member's outcome, in DISPOSITION order — the group reply's row shape. */
export interface CommandGroupMemberResult {
  readonly command_id: string;
  readonly instance_id: string;
  readonly name: string;
  /** The member's state BEFORE the call — what the play rule was decided on. */
  readonly was_running: boolean;
  /** `false` when the gesture deliberately SKIPPED this member (a play over a running one). */
  readonly acted: boolean;
  readonly state: ExecState;
  readonly exit_code: number | null;
  readonly unread: boolean;
  /** This member's own failure, if any. A failing member never aborts the others. */
  readonly error: string | null;
}

/** What `command_group_start` / `_stop` / `_relaunch` reply. */
export interface CommandGroupRunResult {
  readonly group_id: string;
  readonly workspace_id: string;
  readonly action: string;
  readonly members: readonly CommandGroupMemberResult[];
  /** How many members are running AFTER the call — the `n` of the head's `n/m`. */
  readonly running: number;
  readonly total: number;
}

/**
 * Drive a whole group: `(groupId, workspaceId, action)`. Rejects on a whole-call failure (an unknown
 * group, a workspace with no instance of any member); a PER-MEMBER failure comes back inside
 * `members[].error` with the rest of the plan intact.
 */
export function useCommandGroupRun(): (
  groupId: string,
  workspaceId: string,
  action: CommandAction,
) => Promise<CommandGroupRunResult> {
  return useCallback(async (groupId, workspaceId, action) => {
    switch (action) {
      case "start":
        return await nyxBridge.invoke<CommandGroupRunResult>("command_group_start", {
          groupId,
          workspaceId,
        });
      case "stop":
        return await nyxBridge.invoke<CommandGroupRunResult>("command_group_stop", {
          groupId,
          workspaceId,
        });
      case "relaunch":
        return await nyxBridge.invoke<CommandGroupRunResult>("command_group_relaunch", {
          groupId,
          workspaceId,
        });
    }
  }, []);
}

/**
 * THE AGGREGATE STATE OF A GROUP — a dot, a counter, and the three availabilities.
 *
 * ## No fifth `ExecState`
 *
 * A group whose members diverge (one running, one failed, one idle) has no state of its own, and
 * inventing a `partial` token would mean a fifth colour in a vocabulary the whole app reads as four.
 * So the DOT stays one of the four and DIVERGENCE is carried by the `n/m`: `2/3` says more than any
 * colour could, and it is exactly the number the play rule is decided on.
 *
 * WHERE THAT `n/m` IS READ CHANGED, AND {@link CommandGroupTally} DID NOT (human review
 * 01M12MW1DV1V455EWBMV048TE6). The head of a group block carries a STATE and never a number now, so
 * the counter left the sidebar's head for the folder's hover SUMMARY. This tally is still computed
 * on every render — `canStart`/`canStop`/`canRelaunch` are what the three buttons are gated on —
 * and `state` is still the aggregate it always was; it simply no longer has a badge of its own.
 * (The closed folder's DOT is not this `state`: it is `aggregateState` over the members' row states,
 * so a failed member is not masked by a running one — see `workspace-runtime-tree.tsx`.)
 *
 * ## The availabilities are DERIVED here, once
 *
 *  - **play** — live only while at least one member is NOT running (it starts those and only those,
 *    so with none to start it would be a no-op, and a no-op button is a lie);
 *  - **stop** — disabled when nothing runs, exactly as an individual command's stop already is;
 *  - **restart** — always live: it re-spawns the active AND starts the inactive, so it always has
 *    something to do.
 */
export interface CommandGroupTally {
  /** How many members are running. */
  readonly running: number;
  /** How many members the group has an instance for in this workspace. */
  readonly total: number;
  /** The dot's state — one of the SAME four an instance has, never a fifth. */
  readonly state: ExecState;
  readonly canStart: boolean;
  readonly canStop: boolean;
  readonly canRelaunch: boolean;
}

export function commandGroupTally(states: readonly ExecState[]): CommandGroupTally {
  const total = states.length;
  const running = states.filter((state) => state === "running").length;
  // Anything running dominates the dot: a group with a live process reads as live. Below that the
  // settled outcomes rank error over success, and idle is what is left.
  const state: ExecState =
    running > 0
      ? "running"
      : states.some((s) => s === "error")
        ? "error"
        : total > 0 && states.every((s) => s === "success")
          ? "success"
          : "idle";
  return {
    running,
    total,
    state,
    canStart: running < total,
    canStop: running > 0,
    canRelaunch: total > 0,
  };
}
