import { TerminalSquareIcon } from "lucide-react";

import { ProjectSettingsDialog } from "@/components/sidebar/project-settings-dialog";

export interface SystemCommandsDialogProps {
  /** Whether the modal is shown. */
  open: boolean;
  /** The Système singleton's project id — the commands' owner. `null` while the scope is unread. */
  projectId: string | null;
  /** The ABSOLUTE home — the workspace path the command form's folder picker relativizes against. */
  home: string | null;
  /**
   * WHICH command the modal opens on (a command TEMPLATE id), or `null`/omitted for the catalogue
   * summary. The Système page's per-command "Details" passes it: this scope's rows get the same
   * door onto their own definition as a project's, and it is literally the same one.
   */
  initialCommandId?: string | null;
  /** Dismiss (Close / backdrop / Escape). */
  onClose: () => void;
}

/**
 * `<SystemCommandsDialog>` — the Système scope's command MANAGER. It is the app's ONE
 * command/settings modal, [`ProjectSettingsDialog`], with its rail reduced to a single entry.
 *
 * ## Why it is no longer a modal of its own (human review 01M1EP31GWDR6FJEXY6E8CVDZZ, finding Y1)
 *
 * It used to be 104 lines that re-declared the popup, the header, the dismissal and the sizing of
 * the project modal in order to mount the very same body — the shared [`CommandsSection`]. A
 * duplicated SHELL, not duplicated logic, but a real defect all the same: two envelopes to keep in
 * step for one way of managing commands, and this one would have fallen behind the day the other
 * moved. What is left here is the PARAMETERISATION, and it is the whole of the difference.
 *
 * ## What it takes away, and how
 *
 * NO GLOBAL PANE — and the way it is removed is the point. This component wires no
 * `globalSection`, so `Global` is not IN THE RAIL: not a greyed entry, not a disabled form, no
 * entry. The backend refuses BOTH of that pane's controls on the singleton — `update_project` for
 * the rename AND `set_project_resume_agent_sessions` for the resume toggle, both behind
 * `require_user_project` — so a `showRename={false}` would have covered half of it and left the
 * other half erroring on use. That is also the whole of « aucun bouton settings / rename / delete »
 * on the Système side: no surface offers them, rather than a surface offering them greyed.
 *
 * ## WHAT IT DELIBERATELY DOES NOT OFFER EITHER: the package.json import
 *
 * (Review 01M1C7C8K0Q1WBHDQKZ29GH8YJ, findings 01M1C7EP2FHR6BEDS5ESSYGZ2B and
 * 01M1C7EMPGY1YDXHC6F1TJPFWS.) `<CommandsSection>` shows an "Import from package.json" entry when
 * it is given a workspace to scan. This scope is given NONE — `importWorkspaceId` is OMITTED
 * below, not merely empty — and the omission is the feature.
 *
 * The Système workspace's root IS the user's HOME. "The scripts of the root workspace" therefore
 * means walking the user's home folder looking for manifests — meaningless for machine services,
 * and measurably expensive: `pkgjson::discover_package_scripts` over a real HOME took **5.9 s warm,
 * 7.6 s cold** on the reviewer's machine, ON the sidecar's single-threaded request loop, which is
 * where the "the system commands take about ten seconds to appear" report comes from. Every other
 * read queued behind it, `command_list` included, each burning its own 5 s deadline in the main
 * process.
 *
 * It is also a coherence fix: `list_importable_scripts` was already CLOSED to agents on this scope
 * for exactly this reason (`system_scope::SYSTEM_SCOPE_MCP_CAPABILITIES`). Leaving the same walk
 * open to the human and shut to the agent was one surface disagreeing with the other.
 *
 * The import is untouched for USER projects, where the root is a code folder and the scan is what
 * it was designed for.
 *
 * ## What the human still gets
 *
 * CREATE, EDIT, DELETE, REORDER and GROUP — every composition verb of a Système command, through
 * `command_create` / `command_update` / `command_delete` / `command_reorder` / `command_group_*`,
 * all of them on the scope's IPC allow-list. The RUN verbs (start / stop / relaunch) live on the
 * rows of the Système page's tree, where the instance is — a template has nothing to start.
 */
export function SystemCommandsDialog({
  open,
  projectId,
  home,
  initialCommandId,
  onClose,
}: SystemCommandsDialogProps) {
  return (
    <ProjectSettingsDialog
      open={open}
      projectId={projectId}
      projectName="System"
      initialCommandId={initialCommandId}
      // The scope's own name for the modal — the header says it once, so no muted subject repeats
      // it an inch to the right. It is also the stem of `Close system commands`.
      title="System commands"
      subject={null}
      icon={TerminalSquareIcon}
      // NO `globalSection` and NO Categories: the rail is Commands and nothing else. See the
      // header for the Global half; the Categories half is the same reasoning on the other
      // vocabulary — categories classify SPECS (PDR-0017), and this scope holds none, so the
      // section is dropped from the LIST rather than offered onto an empty subject.
      showCategories={false}
      // NO `importWorkspaceId` either: no workspace to scan means no Import entry AND no
      // `command_import_scripts` call — and on this scope that call is a walk of the user's HOME.
      workspacePath={home}
      onClose={onClose}
    />
  );
}
