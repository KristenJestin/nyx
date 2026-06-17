import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type { CommandRecord, ProjectTree } from "@/components/sidebar/use-projects";
import type { ExecState } from "@/components/sidebar/use-terminals";
import type { CommandStatePayload } from "./use-command-state";
import type { InstanceWithTemplate } from "./use-commands";

/**
 * Backend event broadcast on EVERY mutation of a command TEMPLATE, whether it
 * originated in a UI `#[tauri::command]` (`command_create`/`command_update`/
 * `command_delete`/`command_resync_source`/`command_unlink_source`/
 * `command_import_create`) or in an MCP tool (`add_command`/`update_command`/
 * `import_commands`). Mirrors `bridge::COMMANDS_CHANGED_EVENT`. The band re-loads its
 * instances on receipt so a template added/edited/removed on an EXISTING workspace —
 * over MCP OR via the UI — shows up live WITHOUT a manual reload (the band's
 * `workspaceIdsKey` only re-runs on a workspace-set change, never on a template
 * mutation).
 */
const COMMANDS_CHANGED_EVENT = "commands://changed";

/**
 * A loaded command instance enriched with where it lives, so the main pane can
 * mount a `<CommandView>` for the selected one and the sidebar can group rows by
 * workspace.
 */
export interface CommandInstance {
  /** `command_instances.id` (the row the view + lifecycle commands target). */
  id: string;
  workspaceId: string;
  /** Display name (from the joined template). */
  name: string;
  /**
   * The FACTUAL run state (seeded from `last_state`, kept fresh by `command://state`).
   * An acknowledge never changes it — it always reflects the true last-run outcome.
   */
  state: ExecState;
  /**
   * The "unseen result" flag (v4): `true` while a finished run has not been
   * acknowledged. Seeded from the row's `unread`, set when a `command://state`
   * settles (success/error), and cleared by `command://ack`. Drives the settled
   * BADGE while `state` keeps reflecting the factual outcome.
   */
  unread: boolean;
  /** The command line that runs (e.g. `bun run start`) — info bar. */
  command: string;
  /** Resolved run directory (workspace + subfolder), or the workspace path. */
  cwd: string;
  /** package.json provenance (null for a hand-authored command) — info bar. */
  sourceKind: string | null;
  sourcePackageJsonPath: string | null;
  sourceScriptName: string | null;
}

/** Narrow an arbitrary string to a known `ExecState`. */
function asExecState(s: string): ExecState {
  return s === "running" || s === "success" || s === "error" ? s : "idle";
}

export interface UseCommandInstances {
  /** All loaded instances, flat (for lookup by id when mounting the view). */
  instances: CommandInstance[];
  /** Sidebar `CommandRecord`s grouped by `workspace_id` (drives the COMMANDS band). */
  commandsByWorkspace: Map<string, CommandRecord[]>;
  /** Re-load every workspace's instances (e.g. after the modal mutates templates). */
  refresh: () => Promise<void>;
}

/**
 * `useCommandInstances` — load the command INSTANCES for every workspace in the
 * project tree (each via `command_instance_list(workspaceId)`), and keep their
 * run state fresh from `command://state` (filtered by `instanceId`). Exposes:
 *
 *  - `instances`: a flat list the main pane uses to resolve the selected id, and
 *  - `commandsByWorkspace`: sidebar `CommandRecord`s grouped by `workspace_id`,
 *    fed into the COMMANDS subsection so each command shows its run-state dot.
 *
 * Re-loads whenever the set of workspace ids changes (a workspace/project was
 * added/removed); `refresh()` re-loads on demand (after the modal creates/imports
 * /deletes a template, which materializes/removes instances).
 */
export function useCommandInstances(projects: ProjectTree[]): UseCommandInstances {
  const [instances, setInstances] = useState<CommandInstance[]>([]);

  // A stable signature of the workspace ids so the load effect re-runs only when
  // the set actually changes (not on every project-tree identity change).
  const workspaceIdsKey = useMemo(
    () =>
      projects
        .flatMap((p) => p.workspaces.map((w) => w.id))
        .sort()
        .join(","),
    [projects],
  );

  const load = useCallback(async () => {
    const workspaceIds = workspaceIdsKey ? workspaceIdsKey.split(",") : [];
    if (workspaceIds.length === 0) {
      setInstances([]);
      return;
    }
    const lists = await Promise.all(
      workspaceIds.map((wsId) =>
        invoke<InstanceWithTemplate[]>("command_instance_list", { workspaceId: wsId }).catch(
          () => [] as InstanceWithTemplate[],
        ),
      ),
    );
    const flat: CommandInstance[] = [];
    for (const list of lists) {
      for (const inst of list) {
        flat.push({
          id: inst.id,
          workspaceId: inst.workspace_id,
          name: inst.name,
          state: asExecState(inst.last_state),
          unread: inst.unread,
          command: inst.command,
          // The bridge resolves `cwd`; fall back to the workspace path defensively.
          cwd: inst.cwd ?? inst.workspace_path,
          sourceKind: inst.source_kind,
          sourcePackageJsonPath: inst.source_package_json_path,
          sourceScriptName: inst.source_script_name,
        });
      }
    }
    setInstances(flat);
  }, [workspaceIdsKey]);

  useEffect(() => {
    let cancelled = false;
    void load().catch(() => {
      if (!cancelled) setInstances([]);
    });
    return () => {
      cancelled = true;
    };
  }, [load]);

  // Re-load every workspace's instances whenever the backend signals a command
  // template changed. This is the single refresh path BOTH the UI's own mutations and
  // the MCP tools' mutations converge on: a template added/edited/removed on an
  // EXISTING workspace does not change `workspaceIdsKey`, so without this the band
  // would never reflect it (especially an MCP-driven mutation the UI never invoked).
  // StrictMode-safe: the listener is torn down on cleanup and a late resolve after
  // unmount is unlistened immediately.
  useEffect(() => {
    let torndown = false;
    let unlisten: (() => void) | undefined;
    void listen(COMMANDS_CHANGED_EVENT, () => {
      if (torndown) return;
      // A transient load failure leaves the current list; the next event recovers.
      void load().catch(() => {});
    }).then((un) => {
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
  }, [load]);

  // Keep the FACTUAL run state fresh: a `command://state` transition updates the
  // matching instance's `state` in place, so the sidebar dots reflect live runs. A
  // settled transition (success/error) marks the result `unread` (an unseen result);
  // a fresh `running` clears it (the new run is not yet seen-or-unseen). The factual
  // `state` is set on EVERY transition — an acknowledge never arrives here (it is the
  // separate `command://ack` event below), so the dot keeps the factual outcome.
  useEffect(() => {
    let torndown = false;
    let unlisten: (() => void) | undefined;
    void listen<CommandStatePayload>("command://state", (event) => {
      if (torndown) return;
      const { instanceId, state } = event.payload;
      const next = asExecState(state);
      const settled = next === "success" || next === "error";
      setInstances((prev) =>
        prev.map((i) =>
          i.id === instanceId
            ? // A settled run becomes unread; a fresh run clears it; idle leaves it.
              { ...i, state: next, unread: settled ? true : next === "running" ? false : i.unread }
            : i,
        ),
      );
    }).then((un) => {
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
  }, []);

  // The acknowledge channel (v4): `command://ack` clears ONLY the matching instance's
  // `unread` flag — the factual `state` is untouched, so the settled BADGE hides while
  // the dot keeps the true outcome. Decoupled from `command://state` so a UI ack can
  // no longer erase the result an observer (the MCP) reads.
  useEffect(() => {
    let torndown = false;
    let unlisten: (() => void) | undefined;
    void listen<{ instanceId: string }>("command://ack", (event) => {
      if (torndown) return;
      const { instanceId } = event.payload;
      setInstances((prev) =>
        prev.map((i) => (i.id === instanceId ? { ...i, unread: false } : i)),
      );
    }).then((un) => {
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
  }, []);

  const commandsByWorkspace = useMemo(() => {
    const map = new Map<string, CommandRecord[]>();
    for (const inst of instances) {
      const record: CommandRecord = {
        id: inst.id,
        label: inst.name,
        state: inst.state,
        unread: inst.unread,
      };
      const list = map.get(inst.workspaceId);
      if (list) list.push(record);
      else map.set(inst.workspaceId, [record]);
    }
    return map;
  }, [instances]);

  return { instances, commandsByWorkspace, refresh: load };
}
