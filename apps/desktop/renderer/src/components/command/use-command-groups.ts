import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { nyxBridge } from "@/bridge";
import type { BackendCommand } from "@/bridge/contract";
import {
  asCommandGroupShape,
  type CommandGroupShape,
  type GroupColor,
} from "@/components/panes/pane-groups";

/**
 * THE MULTI-PANE GROUPING OF COMMANDS — the renderer's live view of it, and the six edits.
 *
 * ## Why this one lives in the DB and the terminals' one does not
 *
 * A terminal group is a per-machine view preference and sits in `localStorage`. A COMMAND group
 * cannot: the review (01M0ABR6V501D0SPR2BYBVZC4A) requires the AGENT to drive the very same
 * operations over MCP and to produce EXACTLY the state a hand gesture produces, in both directions.
 * MCP is served by the sidecar, which cannot see `localStorage` — so the grouping is persisted in
 * `managed_command_groups` (migration `create_command_groups`), and BOTH sides call the same `db::` transitions. This
 * hook is the UI half: it invokes the IPC routes, and re-reads on `commands://changed`, which those
 * very routes AND the MCP tool broadcast — so an agent's edit repaints the list with no polling.
 *
 * ## The model is the terminals' model
 *
 * A command belongs to zero or one group; a group holds at least two members; removing the
 * second-to-last DISSOLVES it and leaves the survivor solo; `name` and `color` are optional and
 * independent. The DISPOSITION is the ordered member list — `commandIds` — from which the pane
 * tree is derived (`autoLayoutTree`), so re-ordering is re-arranging.
 */

/** Broadcast on every command/template/group mutation, whoever made it (UI or MCP). */
const COMMANDS_CHANGED_EVENT = "commands://changed" as const;

/** A command group as the IPC route serves it: identity, labels, and its ORDERED members. */
export interface CommandGroup {
  readonly id: string;
  readonly project_id: string;
  /** `null` = unnamed — the default, which renders no head at all. */
  readonly name: string | null;
  /** `null` = uncoloured — the neutral default, NOT a seventh colour. */
  readonly color: GroupColor | null;
  /**
   * The TILING SHAPE the ordered members are poured into — one of `COMMAND_GROUP_SHAPES`, `"auto"`
   * being the default and the behaviour of before (the disposition derived from the member count).
   *
   * The column exists since the `20260827120000_command_groups_layout_shape` migration and is `NOT
   * NULL`, so the wire always carries it; the value is nonetheless narrowed through
   * {@link asCommandGroupShape} on the way in rather than trusted, because a string from another
   * process is a string.
   */
  readonly layout_shape: CommandGroupShape;
  /** The member TEMPLATE ids, in DISPOSITION order. */
  readonly command_ids: readonly string[];
}

export interface CommandGroupsController {
  readonly groups: readonly CommandGroup[];
  /** The group holding `commandId`, or `null` when it is SOLO. */
  readonly groupOf: (commandId: string) => CommandGroup | null;
  /** Create a group from at least two commands, in the order they should be laid out. */
  readonly createGroup: (commandIds: readonly string[]) => Promise<void>;
  /** Put a command INTO a group, at the end of its disposition (the automatic position). */
  readonly addToGroup: (groupId: string, commandId: string) => Promise<void>;
  /** Take a command OUT of its group — it becomes solo; the group may dissolve. */
  readonly removeFromGroup: (commandId: string) => Promise<void>;
  /** Dissolve a group — every member becomes solo ("Ungroup all"). */
  readonly dissolveGroup: (groupId: string) => Promise<void>;
  readonly renameGroup: (groupId: string, name: string | null) => Promise<void>;
  readonly recolorGroup: (groupId: string, color: GroupColor | null) => Promise<void>;
  /** Set the disposition: exactly the current members, in the wanted order. */
  readonly setLayout: (groupId: string, commandIds: readonly string[]) => Promise<void>;
  /**
   * Set the TILING SHAPE — WHAT the grid looks like, as opposed to {@link setLayout}'s WHICH member
   * goes where. Two routes and not one because a re-order and a re-shape are two intents and a
   * single call could not tell them apart.
   */
  readonly setShape: (groupId: string, shape: CommandGroupShape) => Promise<void>;
  /**
   * Persist the ORDER OF THE WHOLE COMMAND LIST — every template id of the project, in the order
   * the list now shows them.
   *
   * It belongs to this controller and not to `useCommands` because it has exactly ONE caller: the
   * drop handler of the grouping list, which produces the membership change and the list order in
   * the same gesture and must commit them together. Without it a command dragged OUT of a group
   * sprang back to its creation position — the list had no stored order at all (review
   * 01M11WE6H9E5D5FCS6RT0ZXAXN). The agent's half is the MCP `reorder_commands` tool, over the same
   * `db::reorder_templates`.
   */
  readonly reorderCommands: (commandIds: readonly string[]) => Promise<void>;
  readonly reload: () => Promise<void>;
}

/**
 * The live groups of `projectId` (`null` = none, e.g. no project entered). Re-reads on every
 * `commands://changed`, so an MCP edit and a hand edit repaint identically.
 */
export function useCommandGroups(projectId: string | null): CommandGroupsController {
  const [groups, setGroups] = useState<readonly CommandGroup[]>([]);
  const projectRef = useRef(projectId);
  projectRef.current = projectId;

  const reload = useCallback(async () => {
    const pid = projectRef.current;
    if (pid == null) {
      setGroups([]);
      return;
    }
    try {
      const list = await nyxBridge.invoke<CommandGroup[]>("command_group_list", {
        projectId: pid,
      });
      // A snapshot for ANOTHER project must never land here (the id can change mid-flight). The
      // shape is NARROWED at this boundary — it is the only place a string from the sidecar becomes
      // a `CommandGroupShape`, so nothing downstream has to defend against an unknown one.
      if (projectRef.current === pid) {
        setGroups(
          (list ?? []).map((group) => ({
            ...group,
            layout_shape: asCommandGroupShape(group.layout_shape),
          })),
        );
      }
    } catch {
      // A read that fails is "no memory", never a crash: the list falls back to no group and the
      // commands render as solo rows — exactly what they were before this feature.
      if (projectRef.current === pid) setGroups([]);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [projectId, reload]);

  // LIVE: the same `commands://changed` the template mutations use is broadcast by the group
  // routes AND by the MCP tool, so an agent's grouping shows up without a poll.
  useEffect(() => {
    let torndown = false;
    let unlisten: (() => void) | undefined;
    void nyxBridge
      .subscribe(COMMANDS_CHANGED_EVENT, () => {
        if (!torndown) void reload();
      })
      .then((un) => {
        if (torndown) {
          void Promise.resolve(un()).catch(() => {});
          return;
        }
        unlisten = un;
      });
    return () => {
      torndown = true;
      if (unlisten) void Promise.resolve(unlisten()).catch(() => {});
    };
  }, [reload]);

  /** Every mutation is an IPC round trip followed by a re-read — one source of truth, the DB. */
  const mutate = useCallback(
    async (command: BackendCommand, args: Record<string, unknown>) => {
      await nyxBridge.invoke(command, args);
      await reload();
    },
    [reload],
  );

  return useMemo<CommandGroupsController>(
    () => ({
      groups,
      groupOf: (commandId) => groups.find((group) => group.command_ids.includes(commandId)) ?? null,
      createGroup: (commandIds) =>
        mutate("command_group_create", {
          projectId: projectRef.current,
          commandIds: [...commandIds],
        }),
      addToGroup: (groupId, commandId) => mutate("command_group_add", { groupId, commandId }),
      removeFromGroup: (commandId) => mutate("command_group_remove", { commandId }),
      dissolveGroup: (groupId) => mutate("command_group_dissolve", { groupId }),
      // `name`/`color` are sent EXPLICITLY as `null` to CLEAR — the route tells an absent key
      // ("leave it alone") from an explicit null ("remove it"), which is the difference between
      // not touching the name and deleting it.
      renameGroup: (groupId, name) => mutate("command_group_labels", { groupId, name }),
      recolorGroup: (groupId, color) => mutate("command_group_labels", { groupId, color }),
      setLayout: (groupId, commandIds) =>
        mutate("command_group_layout", { groupId, commandIds: [...commandIds] }),
      setShape: (groupId, shape) => mutate("command_group_shape", { groupId, shape }),
      reorderCommands: (commandIds) => mutate("command_reorder", { ids: [...commandIds] }),
      reload,
    }),
    [groups, mutate, reload],
  );
}
