import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import {
  collidingKeys,
  importableRows,
  importedSourceKeys,
  isAlreadyImported,
  rowKey,
  toRows,
  type ImportRow,
} from "./import-utils";
import type { DiscoveredScript, ManagedCommand } from "./use-commands";

export interface UseImportRows {
  /** Editable rows seeded from the discovered scripts. */
  rows: ImportRow[];
  /** Patch one row (selection / name / command). Already-imported rows are inert. */
  patch: (key: string, next: Partial<ImportRow>) => void;
  /** Select / deselect every IMPORTABLE row of a package.json group. */
  selectGroup: (packageJsonPath: string, selected: boolean) => void;
  /** Source identities already imported into the project (greyed in the UI). */
  importedKeys: Set<string>;
  /** Number of currently-selected rows. */
  selectedCount: number;
  /** The selected, non-colliding rows that are READY to import. */
  ready: ImportRow[];
  /** True when the selection cannot be imported as-is (none selected or a collision). */
  blocked: boolean;
}

/**
 * `useImportRows` — owns the editable import-row state for the "Import from
 * package.json" tab, lifted out of the section so the modal FOOTER can drive the
 * single "Import selected (N)" action (per the tabs layout).
 *
 * Rows seed once from the discovered scripts (re-seeded when the discovered set
 * changes — the dialog keys this hook per workspace open). Already-imported
 * scripts (matched by `(package_json_path, script_name)` of an existing command)
 * are tracked in `importedKeys` and can never be selected: `patch` ignores a
 * select on them, and `selectGroup` only flips importable rows.
 */
export function useImportRows(
  scripts: DiscoveredScript[],
  existingCommands: ManagedCommand[],
  existingNames: string[],
): UseImportRows {
  const [rows, setRows] = useState<ImportRow[]>(() => toRows(scripts));

  // Re-seed the editable rows when the DISCOVERED SET changes (e.g. the modal
  // reopens on another workspace, or a re-list surfaces new scripts). The stable
  // identity is the ordered list of `(path, script)` keys; an in-place edit to a
  // row's name/command/selection does NOT change that identity, so seeding only
  // fires on a genuine discovery change — never clobbering user edits.
  const identity = scripts.map((s) => rowKey(s)).join("|");
  const seededIdentity = useRef(identity);
  useEffect(() => {
    if (seededIdentity.current !== identity) {
      seededIdentity.current = identity;
      setRows(toRows(scripts));
    }
    // `scripts` is intentionally read through the identity gate.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [identity]);

  const importedKeys = useMemo(() => importedSourceKeys(existingCommands), [existingCommands]);

  const patch = useCallback(
    (key: string, next: Partial<ImportRow>) =>
      setRows((prev) =>
        prev.map((r) => {
          if (r.key !== key) return r;
          // Never allow selecting an already-imported row (defense in depth; the
          // checkbox is disabled too).
          if ("selected" in next && isAlreadyImported(r.script, importedKeys)) return r;
          return { ...r, ...next };
        }),
      ),
    [importedKeys],
  );

  const selectGroup = useCallback(
    (packageJsonPath: string, selected: boolean) =>
      setRows((prev) =>
        prev.map((r) =>
          r.script.package_json_path === packageJsonPath &&
          !isAlreadyImported(r.script, importedKeys)
            ? { ...r, selected }
            : r,
        ),
      ),
    [importedKeys],
  );

  const colliding = useMemo(() => collidingKeys(rows, existingNames), [rows, existingNames]);
  const ready = useMemo(() => importableRows(rows, colliding), [rows, colliding]);
  const selectedCount = rows.filter((r) => r.selected).length;
  // Block whenever nothing is selected or ANY selected row collides.
  const blocked = selectedCount === 0 || selectedCount !== ready.length;

  return { rows, patch, selectGroup, importedKeys, selectedCount, ready, blocked };
}
