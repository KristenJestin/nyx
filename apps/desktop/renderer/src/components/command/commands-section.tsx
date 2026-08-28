import { useCallback, useEffect, useMemo, useState } from "react";
import { errorText, nyxBridge } from "@/bridge";

import { toast } from "@/components/ui/toast";
import type { CommandGroupShape, GroupColor } from "@/components/panes/pane-groups";
import { groupListBlocks } from "@/components/panes/group-list-blocks";
import { CommandCatalog } from "./command-catalog";
import { CommandDetail } from "./command-detail";
import { NO_SELECTION, type CommandsSelection } from "./commands-selection";
import { useCommandGroups, type CommandGroup } from "./use-command-groups";
import {
  useCommands,
  type CommandFormValues,
  type DiscoveredScript,
  type ManagedCommand,
} from "./use-commands";
import { useImportRows } from "./use-import-rows";
import { isAlreadyImported, type ImportRow } from "./import-utils";
import type { CommandRow } from "./command-group-tree";

export interface CommandsSectionProps {
  /**
   * Whether the host surface (the dialog/section) is shown. Drives the one-shot
   * load of the importable scripts + lets the host reset the section's transient
   * UI on close. The section itself renders nothing chrome-level — it is the body.
   */
  active: boolean;
  /** The project whose commands are managed. */
  projectId: string | null;
  /** The project's display name — the summary panel names the catalogue by it. */
  projectName?: string;
  /**
   * A workspace of the project to scan for package.json imports (typically the
   * root). `null` hides the import entry. Discovery is per workspace
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

/**
 * `<CommandsSection>` — the chrome-free body of the project COMMANDS surface, and since finding
 * 01M132XFZ5KY8QMJ79HZZSBV3J a MASTER/DETAIL: a catalogue on the left that never moves, a panel on
 * the right that shows whatever is selected.
 *
 * ## What it stopped being
 *
 * It was a stack of tabs and sections that each pushed the others around: a Commands tab holding
 * bordered cards with inline forms, an Import TAB that replaced the list, and — 400 px below the
 * fold — a "MULTI-PANE GROUPING" section drawing THE SAME FIVE COMMANDS AGAIN as gripped rows.
 * The count was written twice (the tab and the footer), a creation opened at the top of the list
 * while an edition opened inside a card, and a group's layout editor was nested inside every block.
 *
 * Everything above is one decision reversed: **there is one list, and one place where what you
 * selected is edited**. The tabs are gone (import is an ENTRY of the list), the footer is gone (the
 * count lives in the catalogue's head), the second list is gone (a group is a folder IN the list),
 * and the four form placements are one panel.
 *
 * ## What it still owns, and only that
 *
 * The two controllers (`useCommands`, `useCommandGroups`), the discovered scripts, the import row
 * state, and the SELECTION. Nothing about layout: `<CommandCatalog>` draws the list and mounts the
 * six gestures, `<CommandDetail>` routes the five panels.
 */
export function CommandsSection({
  active,
  projectId,
  projectName = "this project",
  importWorkspaceId,
  workspacePath,
}: CommandsSectionProps) {
  const { templates, loading, refresh, create, update, remove, resyncSource, unlinkSource } =
    useCommands(projectId);

  /** THE one piece of transient state this screen turns on — see `commands-selection.ts`. */
  const [selection, setSelection] = useState<CommandsSelection>(NO_SELECTION);
  // `formKey` re-mounts the form so it re-seeds from the stored template — after a create, after a
  // source mutation, and after a REFUSED edit (which is how a committed field comes back to what
  // the backend still holds).
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
    setSelection(NO_SELECTION);
    setFormError(null);
    setScripts([]);
  }, [active]);

  const existingNames = useMemo(() => templates.map((t) => t.name), [templates]);

  /**
   * THE MULTI-PANE GROUPING, MOUNTED WHERE THE USER SETTLES IT (finding #90,
   * 01M0ABR6V501D0SPR2BYBVZC4A). A command group is composed and arranged ONLY here — the rendered
   * panes carry no split control and no drag — so this screen is the hand's half of the symmetry
   * with the agent's `manage_command_group` MCP tool. Without this mount there is nothing the hand
   * can do that the agent does, which is exactly what the review forbids.
   *
   * The controller is mounted HERE rather than read from `<CommandGroupsProvider>` on purpose:
   * this body is the "Commands" pane of `<ProjectSettingsDialog>`, which the rail tile opens
   * OUTSIDE that provider — an ambient read would be silently inert there. Two live controllers is
   * not two truths: the rows are `managed_command_groups`, and every mutation (hand or MCP)
   * broadcasts `commands://changed`, which every `useCommandGroups` re-reads on.
   */
  const commandGroups = useCommandGroups(projectId);
  const commandRows: readonly CommandRow[] = useMemo(
    () => templates.map((t) => ({ id: t.id, name: t.name, command: t.command })),
    [templates],
  );

  /**
   * A group's members in DISPOSITION order — the very partition the catalogue draws
   * (`groupListBlocks`), so the panel's preview numbers the panes exactly as the list orders them.
   */
  const membersOf = useCallback(
    (group: CommandGroup): readonly CommandRow[] => {
      const block = groupListBlocks(commandRows, (c) => c.id, commandGroups.groupOf).find(
        (b) => b.kind === "group" && b.group.id === group.id,
      );
      const items = block?.kind === "group" ? block.items : [];
      return [...items].sort(
        (a, b) => group.command_ids.indexOf(a.id) - group.command_ids.indexOf(b.id),
      );
    },
    [commandRows, commandGroups.groupOf],
  );

  // Import-row state lives here so the detail panel can own the "Import selected (N)" action.
  // The hook re-seeds itself when the discovered set changes.
  const importState = useImportRows(scripts, templates, existingNames);
  const importableCount = useMemo(
    () =>
      importWorkspaceId
        ? importState.rows.filter((row) => !isAlreadyImported(row.script, importState.importedKeys))
            .length
        : null,
    [importWorkspaceId, importState.rows, importState.importedKeys],
  );

  const submitForm = useCallback(
    async (id: string | null, values: CommandFormValues) => {
      setSubmitting(true);
      setFormError(null);
      try {
        if (id) await update(id, values);
        else {
          const created = await create(values);
          // The creation lands IN THE LIST and is then the viewed command: the panel does not go
          // blank on success, and what was just created is what is on screen.
          setSelection({ kind: "command", id: created.id });
        }
        toast.success(id ? "Command saved" : "Command created");
      } catch (e) {
        // The error toast carries the REAL backend reason (a refused edit shows e.g. "this command
        // is running in at least one workspace — stop it before editing"). And the form is
        // RE-SEEDED: a field committed on blur comes back to what is stored, which is the accepted
        // cost of having no Save button (`CommandFormCommit`).
        const reason = errorText(e, "Could not save the command.");
        setFormError(reason);
        setFormKey((k) => k + 1);
        toast.error(reason);
      } finally {
        setSubmitting(false);
      }
    },
    [update, create],
  );

  // Delete a command from its line or its panel: re-list happens in the hook; the toast is the
  // feedback (the row vanishes on success, the real reason shows on a refusal).
  const removeCommand = useCallback(
    async (id: string, name: string) => {
      try {
        await remove(id);
        // Nothing is being edited any more — the panel falls back to the summary rather than to a
        // form for something that no longer exists.
        setSelection((current) =>
          current.kind === "command" && current.id === id ? NO_SELECTION : current,
        );
        toast.success(`Deleted “${name}”`);
      } catch (e) {
        toast.error(errorText(e, "Could not delete the command."));
      }
    },
    [remove],
  );

  // Source mutations from the detail panel: resync re-seeds the panel (the command changed);
  // unlink turns it hand-authored. Both re-list already.
  const onResync = useCallback(
    async (id: string) => {
      try {
        await resyncSource(id);
        setFormKey((k) => k + 1);
        toast.success("Command resynced from package.json");
      } catch (e) {
        toast.error(errorText(e, "Could not resync the command."));
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
        toast.error(errorText(e, "Could not unlink the command."));
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
        // Re-list so the imported commands appear IN THE LIST ON THE LEFT and become "already
        // imported" (greyed) here. NOTHING navigates away: the import is a panel, not a tab, so
        // what lands is watched landing — which is exactly what the tab could not do.
        await refresh().catch(() => {});
      } finally {
        setImportSubmitting(false);
      }
    },
    [projectId, refresh],
  );

  return (
    // A DEFINITE HEIGHT, and it is what makes the promise true (finding 01M132XFZ5KY8QMJ79HZZSBV3J
    // — « le modal a une hauteur stable quel que soit le nombre de commandes, et la liste défile
    // toute seule »). Measured in the real app: without it, selecting a command grew the panel past
    // the catalogue, the centred dialog re-centred, and the whole list slid 45 px DOWN — the very
    // "nothing moves under the pointer" this screen is built on, broken by the selection itself.
    // Both columns scroll INSIDE it instead. The mockup states 28rem; it is 32 since the modal lost
    // its footer (human review 01M140VQ4S434P2DMCJSTDR4VM) — the ~4rem band that held a single
    // "Close" is given to the list rather than taken off the dialog, which is where it is worth
    // something: four more command lines before the catalogue has to scroll.
    <div data-commands-section className="flex h-[32rem] min-h-0 min-w-0 flex-1">
      <CommandCatalog
        commands={commandRows}
        controller={commandGroups}
        selection={selection}
        onSelect={setSelection}
        onDelete={(command) => void removeCommand(command.id, command.name)}
        importableCount={importableCount}
      />
      {loading ? (
        <p className="flex-1 p-5 text-xs text-muted-foreground/70 italic">Loading…</p>
      ) : (
        <CommandDetail
          selection={selection}
          onSelect={setSelection}
          projectName={projectName}
          templates={templates as readonly ManagedCommand[]}
          scripts={scripts}
          groups={commandGroups.groups}
          groupOf={commandGroups.groupOf}
          members={membersOf}
          workspacePath={workspacePath}
          submitting={submitting}
          formError={formError}
          formKey={formKey}
          onSubmit={(id, values) => void submitForm(id, values)}
          onDelete={(template) => void removeCommand(template.id, template.name)}
          onResync={onResync}
          onUnlink={onUnlink}
          onRenameGroup={(groupId, name) => void commandGroups.renameGroup(groupId, name)}
          onRecolorGroup={(groupId, color: GroupColor | null) =>
            void commandGroups.recolorGroup(groupId, color)
          }
          onPickShape={(groupId, shape: CommandGroupShape) =>
            void commandGroups.setShape(groupId, shape)
          }
          onDissolveGroup={(groupId) => void commandGroups.dissolveGroup(groupId)}
          importEnabled={Boolean(importWorkspaceId)}
          importState={importState}
          importSubmitting={importSubmitting}
          onImport={(rows) => void importRows(rows)}
        />
      )}
    </div>
  );
}
