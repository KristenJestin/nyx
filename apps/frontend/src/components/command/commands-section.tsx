import { useCallback, useEffect, useMemo, useState } from "react";
import { isBridgeError, nyxBridge } from "@/bridge";
import {
  ChevronRightIcon,
  DownloadIcon,
  PencilIcon,
  PlusIcon,
  TerminalIcon,
  Trash2Icon,
} from "lucide-react";
import { motion, useReducedMotion } from "motion/react";

import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";

/**
 * Extract a human-readable reason from a caught bridge/IPC error. The bridge
 * serializes a backend `Result::Err` into a {@link isBridgeError} whose `message` is
 * the original backend string (e.g. "this command is running in at least one
 * workspace — stop it before editing"); a raw string is passed through; anything else
 * falls back to `fallback`. This is what lets a refused mutation surface its REAL
 * reason in the error toast (FEEDBACK #9), not a generic line.
 */
function errorReason(e: unknown, fallback: string): string {
  return isBridgeError(e) ? e.message : typeof e === "string" ? e : fallback;
}
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Tabs, TabCount } from "@/components/ui/tabs";
import { CollapsibleSection } from "@/components/sidebar/collapsible-section";
import { itemTransition } from "@/components/sidebar/item-motion";
import { CommandForm } from "./command-form";
import { CommandSourceSection } from "./command-source-section";
import { CommandImportSection } from "./command-import-section";
import {
  driftedScriptValue,
  isCustomized,
  useCommands,
  type CommandFormValues,
  type DiscoveredScript,
  type ManagedCommand,
} from "./use-commands";
import { useImportRows } from "./use-import-rows";
import type { ImportRow } from "./import-utils";

export interface CommandsSectionProps {
  /**
   * Whether the host surface (the dialog/section) is shown. Drives the one-shot
   * load of the importable scripts + lets the host reset the section's transient
   * UI on close. The section itself renders nothing chrome-level — it is the body.
   */
  active: boolean;
  /** The project whose commands are managed. */
  projectId: string | null;
  /**
   * A workspace of the project to scan for package.json imports (typically the
   * root). `null` disables the Import tab. Discovery is per workspace
   * (`command_import_scripts(workspaceId)`).
   */
  importWorkspaceId?: string | null;
  /**
   * Absolute path of that workspace. Threaded to `<CommandForm>` so the folder
   * picker can relativize its absolute result into a workspace-relative
   * `subfolder`. `null` when no workspace is known.
   */
  workspacePath?: string | null;
}

type Tab = "commands" | "import";

/**
 * `<CommandsSection>` — the chrome-free body of the project COMMANDS surface
 * (extracted from the former `<ProjectCommandsDialog>` so the SAME UI can live both
 * as a standalone modal and as the "Commands" pane of the project-settings modal,
 * with NO behavioural change). It owns:
 *
 *  1. **Commands** tab — a "New command" affordance over the command list, inline
 *     create/edit IN PLACE (the form expands inside the card), a sourced card's
 *     read-only provenance disclosure, and per-card edit/delete.
 *  2. **Import** tab — `<CommandImportSection>`: scripts grouped per package.json,
 *     already-imported scripts greyed/disabled.
 *
 * A small inline FOOTER carries the per-tab count line and (Import tab only) the
 * "Import selected (N)" action. The host (a dialog) supplies the Close/title chrome
 * around it.
 *
 * Built on Base UI (`Tabs`, `Switch`, `Checkbox`) + Motion, shadcn-style.
 */
export function CommandsSection({
  active,
  projectId,
  importWorkspaceId,
  workspacePath,
}: CommandsSectionProps) {
  const { templates, loading, refresh, create, update, remove, resyncSource, unlinkSource } =
    useCommands(projectId);

  const [tab, setTab] = useState<Tab>("commands");

  // The card being edited in place (null = none). A separate `creating` flag opens
  // a fresh inline form at the top of the list. `formKey` re-mounts the form so it
  // re-seeds from the chosen template on each (re)open / after a source mutation.
  const [editingId, setEditingId] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [formKey, setFormKey] = useState(0);
  const [submitting, setSubmitting] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  // Discovered import scripts for the workspace; loaded when the section activates.
  const [scripts, setScripts] = useState<DiscoveredScript[]>([]);
  const [importSubmitting, setImportSubmitting] = useState(false);

  // Load the importable scripts when the section activates for a workspace.
  useEffect(() => {
    if (!active || !importWorkspaceId) {
      setScripts([]);
      return;
    }
    let cancelled = false;
    void nyxBridge
      .invoke<DiscoveredScript[]>("command_import_scripts", { workspaceId: importWorkspaceId })
      .then((found) => {
        if (!cancelled) setScripts(found);
      })
      .catch(() => {
        if (!cancelled) setScripts([]);
      });
    return () => {
      cancelled = true;
    };
  }, [active, importWorkspaceId]);

  // Reset the transient UI when the host surface closes, so a reopen starts clean
  // without a stale-state intermediate render (same contract as the old modal).
  useEffect(() => {
    if (active) return;
    setTab("commands");
    setEditingId(null);
    setCreating(false);
    setFormError(null);
    setScripts([]);
  }, [active]);

  const existingNames = useMemo(() => templates.map((t) => t.name), [templates]);

  // Import-row state lives here so the inline footer can own the "Import selected (N)"
  // action. The hook re-seeds itself when the discovered set changes.
  const importState = useImportRows(scripts, templates, existingNames);

  const openCreate = useCallback(() => {
    setEditingId(null);
    setCreating(true);
    setFormError(null);
    setFormKey((k) => k + 1);
  }, []);

  const openEdit = useCallback((template: ManagedCommand) => {
    setCreating(false);
    setEditingId(template.id);
    setFormError(null);
    setFormKey((k) => k + 1);
  }, []);

  const closeForm = useCallback(() => {
    setCreating(false);
    setEditingId(null);
    setFormError(null);
  }, []);

  const submitForm = useCallback(
    async (id: string | null, values: CommandFormValues) => {
      setSubmitting(true);
      setFormError(null);
      try {
        if (id) await update(id, values);
        else await create(values);
        closeForm();
        // The toast is now the primary success feedback (the card simply collapses).
        toast.success(id ? "Command saved" : "Command created");
      } catch (e) {
        // The error toast carries the REAL backend reason (FEEDBACK #9: a refused
        // edit shows e.g. "this command is running in at least one workspace — stop
        // it before editing", not the old generic line). A trimmed inline message is
        // kept inside the form for proximity.
        const reason = errorReason(e, "Could not save the command.");
        setFormError(reason);
        toast.error(reason);
      } finally {
        setSubmitting(false);
      }
    },
    [update, create, closeForm],
  );

  // Delete a command from its card: re-list happens in the hook; the toast is the
  // feedback (the row vanishes on success, the real reason shows on a refusal).
  const removeCommand = useCallback(
    async (id: string, name: string) => {
      try {
        await remove(id);
        toast.success(`Deleted “${name}”`);
      } catch (e) {
        toast.error(errorReason(e, "Could not delete the command."));
      }
    },
    [remove],
  );

  // Source mutations from the edit form: resync re-seeds the still-open form (the
  // command changed); unlink turns the card hand-authored. Both re-list already.
  const onResync = useCallback(
    async (id: string) => {
      try {
        await resyncSource(id);
        setFormKey((k) => k + 1);
        toast.success("Command resynced from package.json");
      } catch (e) {
        toast.error(errorReason(e, "Could not resync the command."));
      }
    },
    [resyncSource],
  );
  const onUnlink = useCallback(
    async (id: string) => {
      try {
        await unlinkSource(id);
        setFormKey((k) => k + 1);
        toast.success("Command unlinked from package.json");
      } catch (e) {
        toast.error(errorReason(e, "Could not unlink the command."));
      }
    },
    [unlinkSource],
  );

  const importRows = useCallback(
    async (rows: ImportRow[]) => {
      if (!projectId) return;
      setImportSubmitting(true);
      try {
        // Fire the imports in PARALLEL — each is an independent backend
        // `command_import_create`. Each promise swallows its OWN rejection so one
        // failure never aborts the others; the offending row stays selected for a
        // retry. `Promise.all` over these never-rejecting promises just waits.
        await Promise.all(
          rows.map((row) =>
            nyxBridge
              .invoke("command_import_create", {
                projectId,
                name: row.name.trim(),
                command: row.command.trim(),
                subfolder: row.script.subfolder,
                sourcePackageJsonPath: row.script.package_json_path,
                sourceScriptName: row.script.script_name,
                sourceScriptCommandSnapshot: row.script.script_command_snapshot,
                packageManager: row.script.package_manager,
              })
              .catch(() => {
                // Per-row rejection: the row simply stays selected (it never landed).
              }),
          ),
        );
        // Re-list so the imported commands appear under the Commands tab AND become
        // "already imported" (greyed) in the import table.
        await refresh().catch(() => {});
        // Hop to the Commands tab so the user sees what landed.
        setTab("commands");
      } finally {
        setImportSubmitting(false);
      }
    },
    [projectId, refresh],
  );

  const linkedCount = useMemo(
    () => templates.filter((t) => t.source_script_name && t.source_package_json_path).length,
    [templates],
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <Tabs.Root
        value={tab}
        onValueChange={(v) => setTab(v as Tab)}
        className="flex min-h-0 flex-1 flex-col"
      >
        <Tabs.List>
          <Tabs.Tab value="commands">
            <TerminalIcon />
            Commands
            {templates.length > 0 && <TabCount>{templates.length}</TabCount>}
          </Tabs.Tab>
          {importWorkspaceId && (
            <Tabs.Tab value="import">
              <DownloadIcon />
              Import from package.json
            </Tabs.Tab>
          )}
        </Tabs.List>

        {/* The panels region animates its HEIGHT on a tab switch (review finding).
            `deps={tab}` re-measures on switch. */}
        <div className="min-h-0 flex-1">
          <Tabs.AnimatedHeight deps={tab}>
            {/* === Commands tab ==================================== */}
            <Tabs.AnimatedPanel value="commands" activeValue={tab} className="pt-4 pb-1">
              <div className="mb-3 flex items-center justify-between">
                <span className="text-xs text-muted-foreground">
                  Named commands run per workspace.
                </span>
                <Button type="button" size="xs" onClick={openCreate}>
                  <PlusIcon />
                  New command
                </Button>
              </div>

              {/* Inline create form (at the top, in place). */}
              <CollapsibleSection open={creating}>
                <div className="mb-2 rounded-md border border-primary/40 bg-card p-3">
                  <CommandForm
                    key={`create-${formKey}`}
                    editing={null}
                    workspacePath={workspacePath}
                    submitting={submitting}
                    error={formError}
                    onSubmit={(values) => void submitForm(null, values)}
                    onCancel={closeForm}
                  />
                </div>
              </CollapsibleSection>

              {loading ? (
                <p className="px-1 py-2 text-xs text-muted-foreground/70 italic">Loading…</p>
              ) : templates.length === 0 && !creating ? (
                <EmptyCommands />
              ) : (
                <ul className="flex flex-col gap-2">
                  {templates.map((template) => (
                    <CommandCard
                      key={template.id}
                      template={template}
                      editing={editingId === template.id}
                      driftValue={driftedScriptValue(template, scripts)}
                      workspacePath={workspacePath}
                      submitting={submitting}
                      formError={editingId === template.id ? formError : null}
                      formKey={formKey}
                      onEdit={() => openEdit(template)}
                      onDelete={() => void removeCommand(template.id, template.name)}
                      onSubmit={(values) => void submitForm(template.id, values)}
                      onCancel={closeForm}
                      onResync={onResync}
                      onUnlink={onUnlink}
                    />
                  ))}
                </ul>
              )}
            </Tabs.AnimatedPanel>

            {/* === Import tab ===================================== */}
            {importWorkspaceId && (
              <Tabs.AnimatedPanel value="import" activeValue={tab} className="pt-4 pb-1">
                <p className="mb-3 text-xs text-muted-foreground">
                  Pick scripts to turn into project commands. Already-imported scripts are disabled.
                </p>
                <CommandImportSection
                  rows={importState.rows}
                  onPatchRow={importState.patch}
                  onSelectGroup={importState.selectGroup}
                  existingNames={existingNames}
                  importedKeys={importState.importedKeys}
                />
              </Tabs.AnimatedPanel>
            )}
          </Tabs.AnimatedHeight>
        </div>
      </Tabs.Root>

      {/* === Inline footer (count + Import action) ========================= */}
      <div className="mt-3 flex items-center justify-between gap-2 border-t border-border pt-3.5">
        <span className="text-xs text-muted-foreground">
          {tab === "commands"
            ? `${templates.length} command${templates.length === 1 ? "" : "s"}${
                linkedCount > 0 ? ` · ${linkedCount} linked to package.json` : ""
              }`
            : importState.selectedCount > 0 && importState.blocked
              ? "Resolve name conflicts to import."
              : "Pick scripts to turn into project commands."}
        </span>
        {tab === "import" && importWorkspaceId && (
          <Button
            type="button"
            size="sm"
            loading={importSubmitting}
            disabled={importState.blocked || importSubmitting}
            onClick={() => void importRows(importState.ready)}
          >
            <DownloadIcon />
            Import selected
            {importState.selectedCount > 0 ? ` (${importState.ready.length})` : ""}
          </Button>
        )}
      </div>
    </div>
  );
}

/** The empty state for the Commands tab when a project has no commands yet. */
function EmptyCommands() {
  return (
    <div className="flex flex-col items-center gap-2.5 px-4 py-12 text-center text-muted-foreground">
      <span className="flex size-10 items-center justify-center rounded-xl bg-muted">
        <TerminalIcon className="size-5" />
      </span>
      <p className="text-sm text-foreground">No commands yet</p>
      <small className="text-xs">Create one above, or import from the package.json tab.</small>
    </div>
  );
}

interface CommandCardProps {
  template: ManagedCommand;
  editing: boolean;
  /** Live on-disk script value when the source drifted (else null). */
  driftValue: string | null;
  workspacePath?: string | null;
  submitting: boolean;
  formError: string | null;
  formKey: number;
  onEdit: () => void;
  onDelete: () => void;
  onSubmit: (values: CommandFormValues) => void;
  onCancel: () => void;
  onResync: (id: string) => Promise<void>;
  onUnlink: (id: string) => Promise<void>;
}

/**
 * One command card: name + command + passive badges, an edit/delete action set,
 * a (sourced-only) read-only provenance disclosure, and the **inline edit form
 * IN PLACE** (expands inside the card while `editing`). When editing a sourced
 * command, the form carries the source-control block (Resync / Unlink).
 */
function CommandCard({
  template,
  editing,
  driftValue,
  workspacePath,
  submitting,
  formError,
  formKey,
  onEdit,
  onDelete,
  onSubmit,
  onCancel,
  onResync,
  onUnlink,
}: CommandCardProps) {
  const reduced = useReducedMotion();
  const [sourceOpen, setSourceOpen] = useState(false);
  const hasSource = Boolean(template.source_script_name && template.source_package_json_path);
  const customized = isCustomized(template);

  return (
    <li
      className={cn(
        "overflow-hidden rounded-md border border-border bg-card/40 transition-colors",
        editing ? "border-primary/40 bg-card" : "hover:bg-card",
      )}
    >
      <div className="flex items-center gap-3 px-3 py-2.5">
        <div className="flex min-w-0 flex-1 flex-col">
          <div className="flex flex-wrap items-center gap-2">
            <span className="truncate text-sm font-semibold text-foreground">{template.name}</span>
            {hasSource && (
              <Badge variant="source">
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  aria-hidden
                >
                  <path d="m7.5 4.27 9 5.15M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z" />
                </svg>
                scripts.{template.source_script_name}
              </Badge>
            )}
            {customized && (
              <Badge variant="info" data-testid="customized-badge">
                edited
              </Badge>
            )}
            {hasSource && driftValue != null && (
              <Badge
                variant="warning"
                data-testid="drift-badge"
                title="The script changed in package.json since import. Open Edit to resync."
              >
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  aria-hidden
                >
                  <path d="M12 9v4M12 17h.01M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0Z" />
                </svg>
                changed in package.json
              </Badge>
            )}
          </div>
          <span className="truncate font-mono text-xs text-muted-foreground">
            {template.command}
            {template.subfolder ? (
              <span className="text-muted-foreground/60"> · {template.subfolder}</span>
            ) : null}
          </span>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {hasSource && !editing && (
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label={`Toggle source for ${template.name}`}
              aria-expanded={sourceOpen}
              onClick={() => setSourceOpen((v) => !v)}
            >
              <motion.span
                aria-hidden
                animate={{ rotate: sourceOpen ? 90 : 0 }}
                transition={itemTransition(reduced)}
                className="flex items-center"
              >
                <ChevronRightIcon className="size-3.5" />
              </motion.span>
            </Button>
          )}
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label={`Edit ${template.name}`}
            onClick={onEdit}
          >
            <PencilIcon />
          </Button>
          <Button
            type="button"
            variant="ghost-destructive"
            size="icon-xs"
            aria-label={`Delete ${template.name}`}
            onClick={onDelete}
          >
            <Trash2Icon />
          </Button>
        </div>
      </div>

      {/* Read-only provenance disclosure (sourced + not editing). */}
      {hasSource && !editing && (
        <CollapsibleSection open={sourceOpen}>
          <div className="px-3 pb-3">
            <CommandSourceSection command={template} driftValue={driftValue} />
          </div>
        </CollapsibleSection>
      )}

      {/* Inline edit form IN PLACE. */}
      <CollapsibleSection open={editing}>
        <div className="border-t border-dashed border-border px-3 pt-3 pb-3">
          <CommandForm
            key={`edit-${template.id}-${formKey}`}
            editing={template}
            workspacePath={workspacePath}
            submitting={submitting}
            error={formError}
            driftValue={driftValue}
            onSubmit={onSubmit}
            onCancel={onCancel}
            onResync={onResync}
            onUnlink={onUnlink}
          />
        </div>
      </CollapsibleSection>
    </li>
  );
}
