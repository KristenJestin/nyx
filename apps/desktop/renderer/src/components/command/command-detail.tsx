import type { CSSProperties } from "react";
import { DownloadIcon, PackageIcon, PlusIcon, Trash2Icon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { SidebarLineFamilyGlyph } from "@/components/ui/sidebar-line";
import { MicroLabel } from "@/components/ui/tree-primitives";
import type { CommandGroupShape, GroupColor } from "@/components/panes/pane-groups";
import { CommandForm } from "./command-form";
import { CommandGroupDetail } from "./command-group-detail";
import { CommandImportSection } from "./command-import-section";
import { DetailHeader, DetailSeparator } from "./command-detail-chrome";
import { commandPanePosition, type CommandsSelection } from "./commands-selection";
import type { CommandRow } from "./command-group-tree";
import type { CommandGroup } from "./use-command-groups";
import {
  driftedScriptValue,
  type CommandFormValues,
  type DiscoveredScript,
  type ManagedCommand,
} from "./use-commands";
import type { ImportRow } from "./import-utils";
import type { UseImportRows } from "./use-import-rows";

/**
 * THE DETAIL PANEL — the SLAVE half of the commands screen: it shows WHAT IS SELECTED, and it is
 * the only thing on this screen that changes when the selection does.
 *
 * ## The four placements it replaces (finding 01M132XFZ5KY8QMJ79HZZSBV3J)
 *
 * A creation opened at the top of the list and pushed everything down. An edition opened INSIDE the
 * card, so the list re-flowed around it. A group's name and colour opened in a SECOND DIALOG over
 * the modal. An import replaced the whole list with a tab. Four things that are all "what am I
 * editing", in four places, each of which moved something else.
 *
 * They are one region now, and the catalogue on the left never moves. That is what makes the
 * catalogue readable at any project size — and it is why the import can finally be watched landing
 * in the list it is importing into.
 *
 * ## It routes; it does not decide
 *
 * Every panel below is its own component or its own dozen lines, and the mutations are the caller's
 * — this file owns no state at all. `<CommandsSection>` holds the selection and the controllers,
 * exactly as it held the tab before.
 */
export function CommandDetail({
  selection,
  onSelect,
  projectName,
  templates,
  scripts,
  groups,
  groupOf,
  members,
  workspacePath,
  submitting,
  formError,
  formKey,
  onSubmit,
  onDelete,
  onResync,
  onUnlink,
  onRenameGroup,
  onRecolorGroup,
  onPickShape,
  onDissolveGroup,
  importEnabled,
  importState,
  importSubmitting,
  onImport,
}: {
  selection: CommandsSelection;
  onSelect: (next: CommandsSelection) => void;
  projectName: string;
  templates: readonly ManagedCommand[];
  /** The workspace's discovered package.json scripts — the drift yardstick and the import source. */
  scripts: readonly DiscoveredScript[];
  groups: readonly CommandGroup[];
  groupOf: (commandId: string) => CommandGroup | null;
  /** A group's members, in disposition order — the catalogue's own ordering, handed in. */
  members: (group: CommandGroup) => readonly CommandRow[];
  workspacePath?: string | null;
  submitting: boolean;
  formError: string | null;
  /** Bumped by the caller to RE-SEED the form — after a create, a source action, or a refusal. */
  formKey: number;
  onSubmit: (id: string | null, values: CommandFormValues) => void;
  onDelete: (template: ManagedCommand) => void;
  onResync: (id: string) => Promise<void>;
  onUnlink: (id: string) => Promise<void>;
  onRenameGroup: (groupId: string, name: string | null) => void;
  onRecolorGroup: (groupId: string, color: GroupColor | null) => void;
  onPickShape: (groupId: string, shape: CommandGroupShape) => void;
  onDissolveGroup: (groupId: string) => void;
  importEnabled: boolean;
  importState: UseImportRows;
  importSubmitting: boolean;
  onImport: (rows: ImportRow[]) => void;
}) {
  /** The catalogue's summary — also the fallback whenever a selection stops existing. */
  const summary = (
    <CatalogSummary
      projectName={projectName}
      commandCount={templates.length}
      groupCount={groups.length}
      linkedCount={
        templates.filter((t) => t.source_script_name && t.source_package_json_path).length
      }
    />
  );

  const panel = (() => {
    switch (selection.kind) {
      case "none":
        return summary;

      case "new":
        return (
          <>
            <DetailHeader
              icon={<PlusIcon className="size-4" />}
              title="New command"
              subtitle="It is created outside any folder. You can drag it into one afterwards."
            />
            <CommandForm
              key={`create-${formKey}`}
              editing={null}
              commit="submit"
              workspacePath={workspacePath}
              submitting={submitting}
              error={formError}
              onSubmit={(values) => onSubmit(null, values)}
              onCancel={() => onSelect({ kind: "none" })}
            />
          </>
        );

      case "import":
        return importEnabled ? (
          <>
            <DetailHeader
              icon={<PackageIcon className="size-4" />}
              title="Import from package.json"
              subtitle="The scripts found in the root workspace. Already-imported ones are greyed."
            />
            <CommandImportSection
              rows={importState.rows}
              onPatchRow={importState.patch}
              onSelectGroup={importState.selectGroup}
              existingNames={templates.map((t) => t.name)}
              importedKeys={importState.importedKeys}
            />
            <div className="mt-4 flex items-center gap-2.5">
              <Button
                type="button"
                size="sm"
                loading={importSubmitting}
                disabled={importState.blocked || importSubmitting}
                onClick={() => onImport(importState.ready)}
              >
                <DownloadIcon />
                Import selected
                {importState.selectedCount > 0 ? ` (${importState.ready.length})` : ""}
              </Button>
              <span className="text-xs text-muted-foreground">
                {importState.selectedCount > 0 && importState.blocked
                  ? "Resolve name conflicts to import."
                  : "They appear in the list on the left, without leaving this screen."}
              </span>
            </div>
          </>
        ) : (
          summary
        );

      case "group": {
        const group = groups.find((g) => g.id === selection.id);
        // A folder can DISSOLVE under the selection (its second-to-last member left). The panel
        // falls back to the summary rather than to a blank: nothing is being edited any more.
        if (group == null) return summary;
        return (
          <CommandGroupDetail
            key={group.id}
            group={group}
            members={members(group)}
            onRename={(name) => onRenameGroup(group.id, name)}
            onRecolor={(color) => onRecolorGroup(group.id, color)}
            onPickShape={(shape) => onPickShape(group.id, shape)}
            onDissolve={() => {
              onDissolveGroup(group.id);
              onSelect({ kind: "none" });
            }}
          />
        );
      }

      case "command": {
        const template = templates.find((t) => t.id === selection.id);
        if (template == null) return summary;
        const position = commandPanePosition(groupOf(template.id), template.id);
        const hasSource = Boolean(template.source_script_name && template.source_package_json_path);
        return (
          <>
            <DetailHeader
              // THE COMMAND FAMILY GLYPH, from its one source (human review 01M140VRZJG0RE5VMG91D8TH3T):
              // the panel names the very object the catalogue line on the left names, so it cannot
              // wear a different sign from it.
              icon={<SidebarLineFamilyGlyph family="command" />}
              title={template.name}
              subtitle={
                position == null ? (
                  // OUTSIDE A FOLDER — and the sentence says what that MEANS for the tiling, not
                  // merely that a field is empty.
                  "Outside any folder — it runs on its own, in the full frame."
                ) : (
                  <>
                    In the folder{" "}
                    <span
                      className="font-medium text-[var(--group-tone)]"
                      style={
                        {
                          "--group-tone": `var(--identity-${position.group.color ?? 0})`,
                        } as CSSProperties
                      }
                    >
                      {position.group.name?.trim() || "without a name"}
                    </span>{" "}
                    · pane n° {position.paneNumber}
                  </>
                )
              }
              action={
                <Button
                  type="button"
                  variant="destructive-outline"
                  size="xs"
                  aria-label={`Delete ${template.name}`}
                  onClick={() => onDelete(template)}
                >
                  <Trash2Icon />
                  Delete
                </Button>
              }
            />
            <CommandForm
              key={`edit-${template.id}-${formKey}`}
              editing={template}
              commit="blur"
              workspacePath={workspacePath}
              submitting={submitting}
              error={formError}
              driftValue={driftedScriptValue(template, [...scripts])}
              onSubmit={(values) => onSubmit(template.id, values)}
              onResync={onResync}
              onUnlink={onUnlink}
            />
            {hasSource ? null : (
              <>
                <DetailSeparator />
                <p className="text-xs text-muted-foreground">
                  Hand-written command — no provenance, so nothing to resync.
                </p>
              </>
            )}
          </>
        );
      }
    }
  })();

  return (
    <div
      data-command-detail={selection.kind}
      className="min-h-0 min-w-0 flex-1 overflow-y-auto p-5"
    >
      {panel}
    </div>
  );
}

/**
 * WHAT THE CATALOGUE IS, WHEN NOTHING IS SELECTED — and the one place that says the folders exist.
 *
 * The dotted "Drop a command here to start a group" slot used to ANNOUNCE that groups were a thing.
 * Removing it (human review 01M1313GN2ESW0BCM8S1G0ZY2P) took the announcement with it, and that is
 * the one cost the review flagged. This is where it comes back: at rest, the panel says a line
 * dropped on another makes them run side by side — for free, in space nothing else wanted.
 */
function CatalogSummary({
  projectName,
  commandCount,
  groupCount,
  linkedCount,
}: {
  projectName: string;
  commandCount: number;
  groupCount: number;
  linkedCount: number;
}) {
  return (
    <div
      data-command-summary
      className="flex h-full min-h-[18rem] flex-col items-center justify-center gap-2.5 text-center text-muted-foreground"
    >
      <span className="flex size-10 items-center justify-center rounded-xl bg-muted">
        <SidebarLineFamilyGlyph family="command" className="size-5" />
      </span>
      <p className="text-sm text-foreground">
        The command catalogue of <span className="font-semibold">{projectName}</span>
      </p>
      <small className="max-w-[40ch] text-xs leading-relaxed">
        Pick a command to edit it, a folder to change its layout, or{" "}
        <span className="font-medium text-foreground">drag a line onto another</span> to make them
        run side by side.
      </small>
      <div className="mt-3 flex gap-6">
        <Stat value={commandCount} label="commands" />
        <Stat value={groupCount} label={groupCount === 1 ? "folder" : "folders"} />
        <Stat value={linkedCount} label="linked to package.json" />
      </div>
    </div>
  );
}

function Stat({ value, label }: { value: number; label: string }) {
  return (
    <div className="text-center">
      <div className="text-lg font-semibold text-foreground tabular-nums">{value}</div>
      <MicroLabel>{label}</MicroLabel>
    </div>
  );
}
