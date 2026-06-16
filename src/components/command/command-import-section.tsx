import { useMemo, useState } from "react";
import { CheckIcon, ChevronDownIcon, PackageIcon } from "lucide-react";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { CollapsibleSection } from "@/components/sidebar/collapsible-section";
import { itemTransition } from "@/components/sidebar/item-motion";
import { collidingKeys, groupRows, isAlreadyImported, type ImportRow } from "./import-utils";

export interface CommandImportSectionProps {
  /** Editable import rows (lifted to the parent so the footer owns the action). */
  rows: ImportRow[];
  /** Patch a single row by key (selection / name / command edits). */
  onPatchRow: (key: string, next: Partial<ImportRow>) => void;
  /** Toggle selection for every (importable) row of a package.json group. */
  onSelectGroup: (packageJsonPath: string, selected: boolean) => void;
  /** Existing project template names (drives the blocking collision check). */
  existingNames: string[];
  /**
   * Source identities (`<package_json_path>::<script_name>`) ALREADY imported into
   * the project. A discovered script in this set is GREYED / disabled with "already
   * imported" and is NOT selectable (review T3) — instead of selectable-then-
   * collision-error.
   */
  importedKeys: Set<string>;
}

/**
 * `<CommandImportSection>` — the "Import from package.json" tab body. Scripts are
 * GROUPED by their originating package.json, each rendered as a **collapsible**
 * block: an animated header (chevron + path + a "N importable · M imported"
 * count + the detected package manager) over an editable table with a SELECT-ALL
 * toggle and per-row CHECKBOX + editable NAME + editable COMMAND.
 *
 * ALREADY-IMPORTED scripts (matched by `(package_json_path, script_name)` against
 * an existing project command — `importedKeys`) are shown GREYED with an "already
 * imported" hint and are NOT selectable (review T3). A selected row whose name
 * collides (already a project template, duplicates another selected row, or is
 * empty) shows an inline blocking error; the FOOTER's "Import selected" is gated
 * on the parent (it reads the same colliding set).
 *
 * Presentational/controlled: the rows + selection live in the parent so the modal
 * footer can own the single "Import selected (N)" action (per the tabs layout).
 */
export function CommandImportSection({
  rows,
  onPatchRow,
  onSelectGroup,
  existingNames,
  importedKeys,
}: CommandImportSectionProps) {
  const reduced = useReducedMotion();
  const groups = useMemo(() => groupRows(rows), [rows]);
  const colliding = useMemo(() => collidingKeys(rows, existingNames), [rows, existingNames]);
  // Each group's collapsed/expanded state (default: expanded). Keyed by path.
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});

  if (rows.length === 0) {
    return (
      <p className="px-1 py-6 text-center text-xs text-muted-foreground/70 italic">
        No package.json scripts found in this workspace.
      </p>
    );
  }

  return (
    <div className="flex flex-col gap-2.5">
      {groups.map((group) => {
        const importableRowsInGroup = group.rows.filter(
          (r) => !isAlreadyImported(r.script, importedKeys),
        );
        const importedCount = group.rows.length - importableRowsInGroup.length;
        const allSelected =
          importableRowsInGroup.length > 0 && importableRowsInGroup.every((r) => r.selected);
        const someSelected = importableRowsInGroup.some((r) => r.selected);
        const isCollapsed = collapsed[group.packageJsonPath] ?? false;

        return (
          <div
            key={group.packageJsonPath}
            className="overflow-hidden rounded-md border border-border"
          >
            {/* Animated collapsible header (chevron rotates; body height-collapses). */}
            <button
              type="button"
              aria-expanded={!isCollapsed}
              onClick={() =>
                setCollapsed((prev) => ({
                  ...prev,
                  [group.packageJsonPath]: !isCollapsed,
                }))
              }
              className="flex w-full items-center gap-2.5 bg-muted/60 px-3 py-2.5 text-left transition-colors hover:bg-muted"
            >
              <motion.span
                aria-hidden
                animate={{ rotate: isCollapsed ? -90 : 0 }}
                transition={itemTransition(reduced)}
                className="flex items-center text-muted-foreground"
              >
                <ChevronDownIcon className="size-3.5" />
              </motion.span>
              <PackageIcon className="size-3.5 shrink-0 text-muted-foreground" />
              <span className="min-w-0 flex-1 truncate font-mono text-xs text-foreground">
                {group.subfolder ? `${group.subfolder}/package.json` : "package.json"}
              </span>
              <span className="shrink-0 text-xs text-muted-foreground">
                {importableRowsInGroup.length} importable
                {importedCount > 0 ? ` · ${importedCount} imported` : ""}
              </span>
              <Badge data-testid="package-manager">{group.packageManager}</Badge>
            </button>

            <CollapsibleSection open={!isCollapsed}>
              <div className="border-t border-border">
                {/* Header row with the select-all toggle. */}
                <div className="flex items-center gap-2.5 border-b border-border bg-card px-3 py-2 text-xs font-semibold tracking-wider text-muted-foreground uppercase">
                  <Checkbox
                    checked={allSelected}
                    indeterminate={someSelected && !allSelected}
                    disabled={importableRowsInGroup.length === 0}
                    onCheckedChange={(checked) =>
                      onSelectGroup(group.packageJsonPath, checked === true)
                    }
                    aria-label={`Select all scripts in ${group.subfolder || "root"}`}
                  />
                  <span className="w-36">Name</span>
                  <span className="flex-1">Run command</span>
                </div>

                <ul>
                  {group.rows.map((row) => {
                    const imported = isAlreadyImported(row.script, importedKeys);
                    const collides = row.selected && colliding.has(row.key);
                    return (
                      <li
                        key={row.key}
                        className={cn(
                          "flex flex-col gap-1 border-b border-border px-3 py-2 last:border-b-0",
                          imported && "opacity-45",
                        )}
                      >
                        <div className="flex items-center gap-2.5">
                          <Checkbox
                            checked={imported ? false : row.selected}
                            disabled={imported}
                            onCheckedChange={(checked) =>
                              onPatchRow(row.key, { selected: checked === true })
                            }
                            aria-label={`Select script ${row.script.script_name}`}
                          />
                          <Input
                            value={row.name}
                            disabled={imported}
                            onChange={(e) => onPatchRow(row.key, { name: e.target.value })}
                            aria-label={`Name for ${row.script.script_name}`}
                            className={cn("w-36", collides && "border-destructive")}
                          />
                          <Input
                            value={row.command}
                            disabled={imported}
                            onChange={(e) => onPatchRow(row.key, { command: e.target.value })}
                            aria-label={`Command for ${row.script.script_name}`}
                            className="flex-1 font-mono"
                          />
                          {imported && (
                            <span
                              data-testid="already-imported"
                              className="flex shrink-0 items-center gap-1 text-xs whitespace-nowrap text-muted-foreground"
                            >
                              <CheckIcon className="size-3 text-success" />
                              already imported
                            </span>
                          )}
                        </div>
                        {collides && (
                          <p
                            role="alert"
                            data-testid="collision-error"
                            className="pl-6 text-xs text-destructive"
                          >
                            {row.name.trim() === ""
                              ? "A name is required."
                              : `"${row.name.trim()}" already exists — choose a unique name.`}
                          </p>
                        )}
                      </li>
                    );
                  })}
                </ul>
              </div>
            </CollapsibleSection>
          </div>
        );
      })}
    </div>
  );
}
