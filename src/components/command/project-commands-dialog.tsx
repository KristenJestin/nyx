import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
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
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Dialog, DialogBackdrop, DialogPopup } from "@/components/ui/dialog";
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

export interface ProjectCommandsDialogProps {
  /** Whether the modal is shown (controlled by the sidebar action). */
  open: boolean;
  /** The project whose commands are managed. */
  projectId: string | null;
  projectName: string;
  /**
   * A workspace of the project to scan for package.json imports (typically the
   * root). `null` disables the import tab. The backend discovery is per workspace
   * (`command_import_scripts(workspaceId)`).
   */
  importWorkspaceId?: string | null;
  /**
   * Absolute path of that workspace. Threaded to `<CommandForm>` so the folder
   * picker can relativize its absolute result into a workspace-relative
   * `subfolder` (the only shape the backend accepts). `null` when no workspace is
   * known — the picker then refuses rather than storing an absolute path.
   */
  workspacePath?: string | null;
  /** Dismiss the modal (Close / backdrop / Escape) — neutral, no other action. */
  onClose: () => void;
}

type Tab = "commands" | "import";

/**
 * `<ProjectCommandsDialog>` — the "Project commands" modal (validated `tabs`
 * layout, round v2). Two tabs:
 *
 *  1. **Commands** — a "New command" affordance over the command list. Each card
 *     shows name + command (+ a passive `edited`/drift badge), with **inline edit
 *     IN PLACE** (the edit form expands inside the card — no floating card above
 *     the list). A sourced card exposes its **read-only** provenance under a
 *     disclosure; the source mutations (Resync / Unlink) live in the edit form.
 *  2. **Import** — `<CommandImportSection>`: scripts grouped per package.json in
 *     an **animated collapse**, already-imported scripts greyed/disabled.
 *
 *  Footer: **Close** is always present; **Import selected (N)** appears only on
 *  the Import tab.
 *
 * Built on Base UI (`Tabs`, `Dialog`, `Switch`, `Checkbox`) + Motion (dialog
 * chrome, collapses, the tab underline), in the shadcn-style of the in-house UI.
 */
export function ProjectCommandsDialog({
  open,
  projectId,
  projectName,
  importWorkspaceId,
  workspacePath,
  onClose,
}: ProjectCommandsDialogProps) {
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

  // Discovered import scripts for the workspace; loaded when the modal opens.
  const [scripts, setScripts] = useState<DiscoveredScript[]>([]);
  const [importSubmitting, setImportSubmitting] = useState(false);

  // Load the importable scripts when the modal opens for a workspace.
  useEffect(() => {
    if (!open || !importWorkspaceId) {
      setScripts([]);
      return;
    }
    let cancelled = false;
    void invoke<DiscoveredScript[]>("command_import_scripts", { workspaceId: importWorkspaceId })
      .then((found) => {
        if (!cancelled) setScripts(found);
      })
      .catch(() => {
        if (!cancelled) setScripts([]);
      });
    return () => {
      cancelled = true;
    };
  }, [open, importWorkspaceId]);

  const existingNames = useMemo(() => templates.map((t) => t.name), [templates]);

  // Import-row state lives here so the FOOTER can own the "Import selected (N)"
  // action. The hook re-seeds itself when the discovered set changes.
  const importState = useImportRows(scripts, templates, existingNames);

  // Close the modal AND reset its transient UI in the SAME commit, synchronously,
  // so a reopen starts clean without an intermediate render at the stale state.
  // Doing the reset here (in the close handler) rather than in a `useEffect(open)`
  // avoids the extra stale-UI render the effect forced between commits.
  const handleClose = useCallback(() => {
    setTab("commands");
    setEditingId(null);
    setCreating(false);
    setFormError(null);
    setScripts([]);
    onClose();
  }, [onClose]);

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
      } catch (e) {
        setFormError(typeof e === "string" ? e : "Could not save the command.");
      } finally {
        setSubmitting(false);
      }
    },
    [update, create, closeForm],
  );

  // Source mutations from the edit form: resync re-seeds the still-open form (the
  // command changed); unlink turns the card hand-authored. Both re-list already.
  const onResync = useCallback(
    async (id: string) => {
      await resyncSource(id);
      setFormKey((k) => k + 1);
    },
    [resyncSource],
  );
  const onUnlink = useCallback(
    async (id: string) => {
      await unlinkSource(id);
      setFormKey((k) => k + 1);
    },
    [unlinkSource],
  );

  const importRows = useCallback(
    async (rows: ImportRow[]) => {
      if (!projectId) return;
      setImportSubmitting(true);
      try {
        // Fire the imports in PARALLEL — each is an independent backend
        // `command_import_create` carrying the (edited) name + command + the source
        // metadata. The latency is O(1) IPC round-trips instead of O(N). Each
        // promise swallows its OWN rejection so one failure (e.g. a backend race on
        // a name — refused there too as defense in depth, though the UI already
        // blocked it) never aborts the others; the offending row stays selected for
        // a retry, exactly as before. `Promise.all` over these never-rejecting
        // promises just waits for every import to settle.
        await Promise.all(
          rows.map((row) =>
            invoke("command_import_create", {
              projectId,
              name: row.name.trim(),
              command: row.command.trim(),
              subfolder: row.script.subfolder,
              sourcePackageJsonPath: row.script.package_json_path,
              sourceScriptName: row.script.script_name,
              sourceScriptCommandSnapshot: row.script.script_command_snapshot,
              packageManager: row.script.package_manager,
            }).catch(() => {
              // Per-row rejection: a toast layer is out of scope here, so the row
              // simply stays selected (it never landed) for the user to retry.
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
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        if (!next) handleClose();
      }}
    >
      <Dialog.Portal>
        <DialogBackdrop />
        <DialogPopup className="flex max-h-[calc(100vh-4rem)] w-[min(44rem,calc(100vw-2rem))] flex-col overflow-hidden p-0">
          {/* Head */}
          <div className="px-5 pt-5">
            <Dialog.Title className="flex items-center gap-2 text-base font-semibold">
              <span className="size-1.5 rounded-full bg-primary shadow-[0_0_12px_var(--color-primary)]" />
              Commands — {projectName}
            </Dialog.Title>
            <Dialog.Description className="mt-1 mb-3.5 text-sm text-muted-foreground">
              Define named commands for this project. They run per workspace.
            </Dialog.Description>
          </div>

          <Tabs.Root
            value={tab}
            onValueChange={(v) => setTab(v as Tab)}
            className="flex min-h-0 flex-1 flex-col"
          >
            <Tabs.List className="px-5">
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

            {/* The panels region SCROLLS when it would exceed the popup's max
                height; within that, `Tabs.AnimatedHeight` animates the region's
                HEIGHT on a tab switch so the modal grows/shrinks smoothly instead
                of snapping to the new content height (review finding). `deps={tab}`
                re-measures on switch. */}
            <div className="min-h-0 flex-1 overflow-y-auto">
              <Tabs.AnimatedHeight deps={tab}>
                {/* === Commands tab ==================================== */}
                <Tabs.AnimatedPanel value="commands" activeValue={tab} className="px-5 pt-4 pb-1">
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
                          onDelete={() => void remove(template.id)}
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
                  <Tabs.AnimatedPanel value="import" activeValue={tab} className="px-5 pt-4 pb-1">
                    <p className="mb-3 text-xs text-muted-foreground">
                      Pick scripts to turn into project commands. Already-imported scripts are
                      disabled.
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

          {/* === Footer ========================================= */}
          <div className="flex items-center justify-between gap-2 border-t border-border bg-muted/40 px-5 py-3.5">
            <span className="text-xs text-muted-foreground">
              {tab === "commands"
                ? `${templates.length} command${templates.length === 1 ? "" : "s"}${
                    linkedCount > 0 ? ` · ${linkedCount} linked to package.json` : ""
                  }`
                : importState.selectedCount > 0 && importState.blocked
                  ? "Resolve name conflicts to import."
                  : "Pick scripts to turn into project commands."}
            </span>
            <div className="flex items-center gap-2">
              <Dialog.Close
                render={
                  <Button type="button" variant="outline" size="sm" onClick={handleClose}>
                    Close
                  </Button>
                }
              />
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
        </DialogPopup>
      </Dialog.Portal>
    </Dialog.Root>
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
