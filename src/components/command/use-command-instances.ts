import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type { CommandRecord, ProjectTree } from "@/components/sidebar/use-projects";
import type { ExecState } from "@/components/sidebar/use-terminals";
import type { CommandStatePayload } from "./use-command-state";
import type { InstanceWithTemplate } from "./use-commands";

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
  /** Live run state (seeded from `last_state`, kept fresh by `command://state`). */
  state: ExecState;
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

  // Keep run state fresh: a `command://state` transition updates the matching
  // instance's `state` in place, so the sidebar dots reflect live runs.
  useEffect(() => {
    let torndown = false;
    let unlisten: (() => void) | undefined;
    void listen<CommandStatePayload>("command://state", (event) => {
      if (torndown) return;
      const { instanceId, state } = event.payload;
      setInstances((prev) =>
        prev.map((i) => (i.id === instanceId ? { ...i, state: asExecState(state) } : i)),
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
      const record: CommandRecord = { id: inst.id, label: inst.name, state: inst.state };
      const list = map.get(inst.workspaceId);
      if (list) list.push(record);
      else map.set(inst.workspaceId, [record]);
    }
    return map;
  }, [instances]);

  return { instances, commandsByWorkspace, refresh: load };
}
