import type { DiscoveredScript, ManagedCommand } from "./use-commands";

/**
 * One editable import row: a discovered script plus the user-editable `name` /
 * `command` and the `selected` (checkbox) flag. Keyed by a stable `key` so React
 * lists / per-row edits are stable across re-renders.
 */
export interface ImportRow {
  /** Stable identity: `<package_json_path>::<script_name>`. */
  key: string;
  script: DiscoveredScript;
  selected: boolean;
  /** Editable proposed name (seeded from `script.proposed_name`). */
  name: string;
  /** Editable command (seeded from `script.default_command`). */
  command: string;
}

/** A group of import rows sharing one package.json (subfolder + manager). */
export interface ImportGroup {
  /** The package.json's subfolder relative to the workspace (`""` = root). */
  subfolder: string;
  /** Absolute package.json path (the group identity / display link). */
  packageJsonPath: string;
  /** Detected package manager (npm/pnpm/yarn/bun) for the whole group. */
  packageManager: string;
  rows: ImportRow[];
}

/** Build the stable per-row key from a discovered script. */
export function rowKey(script: DiscoveredScript): string {
  return `${script.package_json_path}::${script.script_name}`;
}

/** The provenance identity of an import source: its package.json path + script. */
function sourceKey(packageJsonPath: string, scriptName: string): string {
  return `${packageJsonPath}::${scriptName}`;
}

/**
 * The set of source identities (`<package_json_path>::<script_name>`) ALREADY
 * IMPORTED into the project — i.e. matched by `source_package_json_path` +
 * `source_script_name` of an existing project command. A discovered script whose
 * `rowKey` is in this set is shown GREYED / disabled with "already imported"
 * (not selectable), rather than offered for selection only to fail later on a
 * name collision.
 */
export function importedSourceKeys(commands: Iterable<ManagedCommand>): Set<string> {
  const keys = new Set<string>();
  for (const cmd of commands) {
    if (cmd.source_package_json_path && cmd.source_script_name) {
      keys.add(sourceKey(cmd.source_package_json_path, cmd.source_script_name));
    }
  }
  return keys;
}

/** Whether a discovered script's source is already imported (greyed in the UI). */
export function isAlreadyImported(script: DiscoveredScript, imported: Set<string>): boolean {
  return imported.has(rowKey(script));
}

/** Seed editable import rows from the backend-discovered scripts. */
export function toRows(scripts: DiscoveredScript[]): ImportRow[] {
  return scripts.map((script) => ({
    key: rowKey(script),
    script,
    selected: false,
    name: script.proposed_name,
    command: script.default_command,
  }));
}

/**
 * Group rows by their originating package.json (subfolder + manager), preserving
 * the discovery order of the first row in each group. The import section renders
 * one collapsible group per package.json with a select-all toggle.
 */
export function groupRows(rows: ImportRow[]): ImportGroup[] {
  const groups = new Map<string, ImportGroup>();
  const order: string[] = [];
  for (const row of rows) {
    const path = row.script.package_json_path;
    let group = groups.get(path);
    if (!group) {
      group = {
        subfolder: row.script.subfolder,
        packageJsonPath: path,
        packageManager: row.script.package_manager,
        rows: [],
      };
      groups.set(path, group);
      order.push(path);
    }
    group.rows.push(row);
  }
  return order.map((path) => groups.get(path)!);
}

/**
 * Compute the BLOCKING name collisions across selected import rows.
 *
 * A selected row's name is a collision when it is empty, OR it already exists in
 * the project's existing template names, OR it duplicates the name of ANOTHER
 * SELECTED row (two imports cannot share one project-unique name). An unselected
 * row never blocks. Returns the set of colliding row keys, so the section can show
 * an inline error per row and disable Import while non-empty.
 *
 * @param rows the editable import rows
 * @param existingNames the project's current template names (case-sensitive,
 *   matching the backend UNIQUE(project_id, name))
 */
export function collidingKeys(rows: ImportRow[], existingNames: Iterable<string>): Set<string> {
  const existing = new Set(existingNames);
  // Count selected rows per (trimmed) name to find intra-selection duplicates.
  const counts = new Map<string, number>();
  for (const row of rows) {
    if (!row.selected) continue;
    const name = row.name.trim();
    counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  const colliding = new Set<string>();
  for (const row of rows) {
    if (!row.selected) continue;
    const name = row.name.trim();
    if (name === "" || existing.has(name) || (counts.get(name) ?? 0) > 1) {
      colliding.add(row.key);
    }
  }
  return colliding;
}

/** The selected rows that are READY to import (selected and not colliding). */
export function importableRows(rows: ImportRow[], colliding: Set<string>): ImportRow[] {
  return rows.filter((row) => row.selected && !colliding.has(row.key));
}
