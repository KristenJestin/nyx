import { useCallback, useEffect, useRef, useState } from "react";
import { nyxBridge } from "@/bridge";

import type { ExecState } from "@/components/sidebar/use-terminals";

/**
 * Backend event broadcast on EVERY mutation of a command TEMPLATE, whether it
 * originated in a UI `#[tauri::command]` (`command_create`/`command_update`/
 * `command_delete`/`command_resync_source`/`command_unlink_source`/
 * `command_import_create`) or in an MCP tool (`add_command`/`update_command`/
 * `import_commands`). Mirrors `bridge::COMMANDS_CHANGED_EVENT`. The modal re-lists its
 * templates on receipt so a template mutated by another surface (notably an MCP tool
 * the modal never invoked) shows up live WITHOUT a manual reload — the modal otherwise
 * only re-lists on a `projectId` change.
 */
const COMMANDS_CHANGED_EVENT = "commands://changed" as const;

/**
 * Front mirror of the backend `db::ManagedCommand` (a project command template).
 * Snake_case fields match the serialized IPC payload. `order_index` is the SQL
 * `"order"` column; the `source_*` group + `package_manager` are the optional
 * package.json provenance.
 */
export interface ManagedCommand {
  id: string;
  project_id: string;
  name: string;
  command: string;
  subfolder: string | null;
  restart_on_startup: boolean;
  order_index: number;
  created_at: number;
  updated_at: number;
  source_kind: string | null;
  source_package_json_path: string | null;
  source_script_name: string | null;
  source_script_command_snapshot: string | null;
  package_manager: string | null;
}

/** Front mirror of `db::InstanceWithTemplate` — one workspace's command instance. */
export interface InstanceWithTemplate {
  id: string;
  command_id: string;
  workspace_id: string;
  /** The FACTUAL run state (idle|running|success|error). An acknowledge never changes it. */
  last_state: ExecState;
  scrollback: string;
  was_running_on_shutdown: boolean;
  created_at: number;
  updated_at: number;
  /**
   * v4 factual-outcome columns: the last completed run's natural exit code + finish
   * time (null while never-finished/running), and `unread` — the separate "unseen
   * result" flag a UI acknowledge clears WITHOUT touching the factual outcome above.
   */
  last_exit_code: number | null;
  ended_at: number | null;
  unread: boolean;
  // Joined template fields.
  name: string;
  command: string;
  subfolder: string | null;
  order_index: number;
  // Joined source provenance (null for a hand-authored template).
  source_kind: string | null;
  source_package_json_path: string | null;
  source_script_name: string | null;
  package_manager: string | null;
  /** The instance's workspace path (the run-dir base). */
  workspace_path: string;
  /** Resolved run directory (workspace + subfolder), filled by the bridge. */
  cwd: string | null;
}

/** Front mirror of `pkgjson::DiscoveredScript` — one importable package.json script. */
export interface DiscoveredScript {
  proposed_name: string;
  script_name: string;
  default_command: string;
  script_command_snapshot: string;
  subfolder: string;
  package_json_path: string;
  package_manager: string;
}

/** Result of `command_source_refresh`: a freshness status + the (maybe new) snapshot. */
export interface SourceRefreshResult {
  status: string;
  snapshot: string | null;
}

/** Fields a create / edit submits (the editable template surface). */
export interface CommandFormValues {
  name: string;
  command: string;
  subfolder: string;
  restart_on_startup: boolean;
}

/**
 * The RUNNER invocation a package manager uses for `script` — the front mirror of
 * `pkgjson::PackageManager::run_script`. Used to decide whether a command is
 * `customized` (no longer the detected runner call). Defaults to the npm form for
 * an unknown manager (matching the backend fallback).
 */
export function runnerCommand(packageManager: string | null, script: string): string {
  switch (packageManager) {
    case "pnpm":
      return `pnpm ${script}`;
    case "yarn":
      return `yarn ${script}`;
    case "bun":
      return `bun run ${script}`;
    case "npm":
    default:
      return `npm run ${script}`;
  }
}

/**
 * Whether a template's `command` no longer matches EITHER the detected runner
 * call (`pnpm dev`, …) OR the current raw script snapshot — i.e. the user edited
 * it away from its source. Returns `false` for a hand-authored (un-sourced)
 * template (nothing to drift from). This is the UI `customized` badge predicate
 * (the Impl Decision: "marque la commande comme customized si command ne
 * correspond ni à l'appel script détecté ni à la commande brute courante").
 */
export function isCustomized(cmd: ManagedCommand): boolean {
  if (!cmd.source_script_name) return false; // not sourced → never "customized"
  const runner = runnerCommand(cmd.package_manager, cmd.source_script_name);
  const raw = cmd.source_script_command_snapshot;
  if (cmd.command === runner) return false;
  if (raw != null && cmd.command === raw) return false;
  return true;
}

/**
 * Whether a sourced template has DRIFTED from its package.json — i.e. the script
 * body currently on disk differs from the snapshot the command was last synced
 * to. `discovered` is the live `command_import_scripts` result for the workspace;
 * we match the command's source by `(package_json_path, script_name)`. Returns
 * the live on-disk value when drifted, else `null` (no source / script gone /
 * still in sync). Drift is PASSIVE: it drives a "changed in package.json" badge,
 * never an implicit rewrite — adopting the new value is the explicit Resync.
 */
export function driftedScriptValue(
  cmd: ManagedCommand,
  discovered: DiscoveredScript[],
): string | null {
  if (!cmd.source_script_name || !cmd.source_package_json_path) return null;
  const match = discovered.find(
    (s) =>
      s.package_json_path === cmd.source_package_json_path &&
      s.script_name === cmd.source_script_name,
  );
  if (!match) return null; // script no longer discoverable → not a drift signal here
  if (match.script_command_snapshot === cmd.source_script_command_snapshot) return null;
  return match.script_command_snapshot;
}

export interface UseCommands {
  /** The project's command templates, in sidebar order. */
  templates: ManagedCommand[];
  /** True until the initial template list resolves. */
  loading: boolean;
  /** Re-list the project's templates from the backend. */
  refresh: () => Promise<void>;
  /** Create a manual template; re-lists on success. Rejects with the backend error. */
  create: (values: CommandFormValues) => Promise<ManagedCommand>;
  /** Update a template's editable fields; re-lists on success. */
  update: (id: string, values: CommandFormValues) => Promise<void>;
  /** Delete a template (+ its instances); re-lists on success. */
  remove: (id: string) => Promise<void>;
  /** Refresh a template's source snapshot/status (NEVER changes `command`). */
  refreshSource: (id: string) => Promise<SourceRefreshResult>;
  /**
   * RESYNC `command` to the source script's CURRENT raw body (re-read at click
   * time) while KEEPING the package.json link; re-lists. This is the only source
   * action that rewrites `command` without detaching.
   */
  resyncSource: (id: string) => Promise<void>;
  /** Detach the package.json source (clears source_* fields); re-lists. */
  unlinkSource: (id: string) => Promise<void>;
}

/**
 * `useCommands` — the project command-template surface behind the "Manage
 * commands" modal (T10). Loads the templates on mount (and on `projectId`
 * change), and exposes the create / edit / delete mutations plus the source
 * actions (refresh / resync / unlink), each a thin wrapper over the Phase-3
 * Tauri commands, re-listing after a mutation so the modal reflects the new
 * state. (Adopting a new script value is `resync` — it keeps the link; editing
 * the command manually then saving detaches the source in the backend
 * `command_update`. There is no "reset to script runner" action.)
 */
export function useCommands(projectId: string | null): UseCommands {
  const [templates, setTemplates] = useState<ManagedCommand[]>([]);
  const [loading, setLoading] = useState(true);
  const projectRef = useRef(projectId);
  projectRef.current = projectId;

  const refresh = useCallback(async () => {
    const pid = projectRef.current;
    if (!pid) {
      setTemplates([]);
      return;
    }
    const list = await nyxBridge.invoke<ManagedCommand[]>("command_list", { projectId: pid });
    setTemplates(list);
  }, []);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    void (async () => {
      try {
        await refresh();
      } catch {
        if (!cancelled) setTemplates([]);
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [projectId, refresh]);

  // Re-list the templates whenever the backend signals a command template changed.
  // This is the single refresh path BOTH the UI's own mutations and the MCP tools'
  // mutations converge on: the modal otherwise only re-lists on a `projectId` change,
  // so a template mutated by another surface (notably an MCP tool the modal never
  // invoked) would never appear live. StrictMode-safe: the listener is torn down on
  // cleanup and a late resolve after unmount is unlistened immediately.
  useEffect(() => {
    let torndown = false;
    let unlisten: (() => void) | undefined;
    void nyxBridge.subscribe(COMMANDS_CHANGED_EVENT, () => {
      if (torndown) return;
      // A transient list failure leaves the current templates; the next event recovers.
      void refresh().catch(() => {});
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
  }, [refresh]);

  const create = useCallback(
    async (values: CommandFormValues) => {
      const pid = projectRef.current;
      if (!pid) throw new Error("no project");
      const created = await nyxBridge.invoke<ManagedCommand>("command_create", {
        projectId: pid,
        name: values.name,
        command: values.command,
        subfolder: values.subfolder.trim() ? values.subfolder.trim() : null,
        restartOnStartup: values.restart_on_startup,
      });
      await refresh();
      return created;
    },
    [refresh],
  );

  const update = useCallback(
    async (id: string, values: CommandFormValues) => {
      await nyxBridge.invoke("command_update", {
        id,
        name: values.name,
        command: values.command,
        subfolder: values.subfolder.trim() ? values.subfolder.trim() : null,
        restartOnStartup: values.restart_on_startup,
      });
      await refresh();
    },
    [refresh],
  );

  const remove = useCallback(
    async (id: string) => {
      await nyxBridge.invoke("command_delete", { id });
      await refresh();
    },
    [refresh],
  );

  const refreshSource = useCallback(
    async (id: string) => {
      const result = await nyxBridge.invoke<SourceRefreshResult>("command_source_refresh", { id });
      // The snapshot may have changed → re-list so the displayed snapshot updates.
      await refresh();
      return result;
    },
    [refresh],
  );

  const resyncSource = useCallback(
    async (id: string) => {
      await nyxBridge.invoke("command_resync_source", { id });
      await refresh();
    },
    [refresh],
  );

  const unlinkSource = useCallback(
    async (id: string) => {
      await nyxBridge.invoke("command_unlink_source", { id });
      await refresh();
    },
    [refresh],
  );

  return {
    templates,
    loading,
    refresh,
    create,
    update,
    remove,
    refreshSource,
    resyncSource,
    unlinkSource,
  };
}
