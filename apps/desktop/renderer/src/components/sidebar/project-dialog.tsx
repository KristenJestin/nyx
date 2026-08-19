import { useState } from "react";
import { Input } from "@base-ui/react/input";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Dialog, DialogBackdrop, DialogPopup } from "@/components/ui/dialog";
import { Switch } from "@/components/ui/switch";

/** Which flow the project dialog is driving. */
export type ProjectDialogMode = "create" | "edit" | "delete";

export interface ProjectDialogProps {
  /** Whether the dialog is shown (controlled). */
  open: boolean;
  /** The flow: create a new project, rename one, or delete one. */
  mode: ProjectDialogMode;
  /**
   * The picked folder path (CREATE only) — shown read-only; the backend
   * normalizes it. Empty for edit/delete.
   */
  path?: string;
  /** Default/initial display name (folder basename for create, current for edit). */
  defaultName: string;
  /**
   * WHICH entity the flow acts on (default `"project"`). `"workspace"` reuses the very same
   * dialog shell — no second modal to keep in sync — and only swaps the title, the explanatory
   * copy, the field label and the confirm-button label to talk about a single workspace:
   *  - with `mode="delete"`, the destructive confirm for ONE workspace;
   *  - with `mode="edit"`, the workspace RENAME (the flow a worktree had no entry point for at
   *    all, review `01M04WQEQDDPH7NQEK9MPYCXKS`).
   * `create` is always a project (a workspace is created by `<AddWorkspaceDialog>`, which owns
   * the folder-pick step).
   */
  entity?: "project" | "workspace";
  /** A submission error to surface inline. */
  error?: string | null;
  /** True while the create/edit/delete command is in flight. */
  submitting?: boolean;
  /**
   * EDIT only — the project's current `resume_agent_sessions` opt-in (PRD-5 #5).
   * When provided alongside [`onResumeAgentSessionsChange`], the edit dialog shows a
   * toggle that resumes the project's terminals' active agent sessions at relaunch.
   */
  resumeAgentSessions?: boolean;
  /**
   * EDIT only — persist a change to the resume opt-in. Called as the user flips the
   * toggle (independent of the name Save, so the toggle takes effect immediately).
   */
  onResumeAgentSessionsChange?: (resume: boolean) => void;
  /** Confirm: create/edit pass the (edited) name; delete ignores it. */
  onConfirm: (name: string) => void;
  /** Dismiss without acting (cancel / backdrop / Escape). */
  onCancel: () => void;
}

/**
 * `<ProjectDialog>` — the create / edit / delete flow for a project, mirroring
 * `<AddWorkspaceDialog>` (folder → editable name → confirm) but with three modes:
 *
 *  - **create**: shows the picked folder (read-only) + an editable NAME
 *    (pre-filled with the folder basename) and creates on confirm.
 *  - **edit**: just the editable NAME (rename the project's display label).
 *  - **delete**: a destructive CONFIRMATION step — explains that the project and
 *    its workspaces are removed, that its running managed commands are stopped, but
 *    that its terminals survive (become loose), and requires an explicit
 *    "Delete project" click.
 *
 * `entity="workspace"` re-uses the SAME shell for the two single-workspace flows (rename +
 * remove) rather than adding a second dialog to keep in sync: only the wording changes, and the
 * remove copy states outright that nyx forgets the folder without deleting it.
 *
 * THE TWO DELETE COPIES ARE NOT INTERCHANGEABLE (review `01M0A7QGBK43MZB10RF51QR6TS`). They
 * describe two commands that treat the live runtime DIFFERENTLY on the point that matters to
 * the user — the terminals. Both stop the container's running managed commands (`delete_project`
 * gained that kill in review `01M0AJ4F6RMS4CHWQ5B6DB47RC`: the FK cascade erases the
 * `command_instances` rows that ARE the stop handles, so the bridge must kill BEFORE
 * delegating), but only `workspace_delete` closes the terminal PTYs and deletes their records —
 * `delete_project` leaves every terminal alive and merely DETACHED (`ON DELETE SET NULL`). The
 * workspace copy was once a verbatim paste of the project one and therefore promised, on an
 * irreversible confirm, that nothing would be lost. Read what the command executes before
 * touching either sentence; `project-dialog.test.tsx` asserts both directions.
 *
 * Built on the shared ANIMATED `Dialog` primitives (Base UI + a smooth, reduced-
 * motion-aware enter/exit), styled with the design-system tokens like
 * `AddWorkspaceDialog`.
 */
export function ProjectDialog({
  open,
  mode,
  path,
  defaultName,
  entity = "project",
  error,
  submitting = false,
  resumeAgentSessions,
  onResumeAgentSessionsChange,
  onConfirm,
  onCancel,
}: ProjectDialogProps) {
  // Editable name, initialized from `defaultName`. The dialog is remounted per
  // flow (keyed in the caller) so each open starts from the right default.
  const [name, setName] = useState(defaultName);

  const trimmed = name.trim();
  const isDelete = mode === "delete";
  const isWorkspaceDelete = isDelete && entity === "workspace";
  /** The workspace RENAME — the same edit shell, worded for a workspace. */
  const isWorkspaceEdit = mode === "edit" && entity === "workspace";
  const canSubmit = isDelete ? !submitting : trimmed.length > 0 && !submitting;

  const title =
    mode === "create"
      ? "Add project"
      : mode === "edit"
        ? isWorkspaceEdit
          ? "Rename workspace"
          : "Rename project"
        : isWorkspaceDelete
          ? "Remove workspace"
          : "Delete project";
  const confirmLabel =
    mode === "create"
      ? "Add project"
      : mode === "edit"
        ? "Save"
        : isWorkspaceDelete
          ? "Remove workspace"
          : "Delete";

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        if (!next) onCancel();
      }}
    >
      <Dialog.Portal>
        <DialogBackdrop />
        <DialogPopup>
          <Dialog.Title className="text-base font-semibold">{title}</Dialog.Title>

          {isDelete ? (
            <>
              <Dialog.Description className="mt-1 text-sm text-muted-foreground">
                {isWorkspaceDelete ? (
                  <>
                    Remove the workspace{" "}
                    <span className="font-medium text-foreground">{defaultName}</span> from this
                    project.{" "}
                    {/* WHAT `workspace_delete` REALLY DOES (review `01M0A7QGBK43MZB10RF51QR6TS`).
                        This used to promise "Any open terminals are kept (they just become loose,
                        unattached terminals) — nothing is closed" — a sentence inherited verbatim
                        from the Delete-project dialog, where it IS true. On a workspace it is the
                        opposite of the truth: the bridge STOPS every running command instance and
                        CLOSES every interactive terminal PTY of the workspace before delegating
                        (`db_command.rs`, `workspace_delete`), and the use case then DELETES the
                        terminal records + their agent sessions inside the removal transaction
                        (`usecase::remove_workspace`) rather than letting the SET NULL FK detach
                        them. So the user was clicking an irreversible confirm believing they
                        would lose nothing. */}
                    <span className="font-medium text-foreground">
                      Its open terminals are closed and its running commands are stopped
                    </span>{" "}
                    — the terminals, their history and their agent sessions go with it.{" "}
                    {/* Said explicitly because the word "remove" on a worktree invites exactly the
                        wrong reading: this DETACHES the nyx entry, it never touches Git or the
                        disk. Removing the folder (a real `git worktree remove`) is a separate
                        capability nyx does not have. */}
                    <span className="font-medium text-foreground">
                      The folder on disk is not deleted
                    </span>{" "}
                    — nyx only forgets it.
                  </>
                ) : (
                  <>
                    Delete <span className="font-medium text-foreground">{defaultName}</span> and
                    its workspaces.{" "}
                    <span className="font-medium text-foreground">
                      Its running commands are stopped
                    </span>
                    , but any open terminals are kept (they just become loose, unattached
                    terminals) — no terminal is closed.
                    {/* VERIFIED against what `delete_project` executes, not copied from the
                        workspace branch (the copy in the OTHER direction is what produced the
                        false workspace text above). Two DIFFERENT halves:

                        • COMMANDS — the bridge now STOPS every command instance of the project
                          before delegating (`db_command.rs`, `delete_project`, review
                          `01M0AJ4F6RMS4CHWQ5B6DB47RC`). It has to: the delete cascades through
                          `workspaces.project_id` → `command_instances.workspace_id`, erasing the
                          very rows that are the stop handles, so an unkilled command would
                          outlive its project unreachable. That kill is user-visible, hence the
                          added clause — worded for THIS command, never borrowed from the
                          workspace sentence above.
                        • TERMINALS — untouched, and deliberately so: no `pty.close_record` runs
                          here, and each terminal's `workspace_id` is `ON DELETE SET NULL`
                          (`00000000000002_create_projects_workspaces`), so the records SURVIVE,
                          detached — "kept, loose, no terminal closed". Attested by nyx-core's
                          `delete_project_removes_workspaces_and_detaches_but_keeps_terminals`.

                        The old blanket "nothing is closed" was dropped because the command half
                        made it false; the terminal promise it protected is restated narrowly. */}
                  </>
                )}
              </Dialog.Description>
              {error && (
                <p role="alert" className="mt-3 text-sm text-destructive">
                  {error}
                </p>
              )}
              <div className="mt-4 flex justify-end gap-2">
                <Button type="button" variant="outline" size="sm" onClick={onCancel}>
                  Cancel
                </Button>
                <Button
                  type="button"
                  variant="destructive"
                  size="sm"
                  loading={submitting}
                  disabled={!canSubmit}
                  onClick={() => onConfirm(trimmed)}
                >
                  {confirmLabel}
                </Button>
              </div>
            </>
          ) : (
            <>
              <Dialog.Description className="mt-1 text-sm text-muted-foreground">
                {mode === "create"
                  ? "Add a folder as a project."
                  : isWorkspaceEdit
                    ? "Change the workspace's display name. The folder and its branch are untouched."
                    : "Change the project's display name."}
              </Dialog.Description>

              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  if (canSubmit) onConfirm(trimmed);
                }}
                className="mt-4 flex flex-col gap-3"
              >
                {mode === "create" && path !== undefined && (
                  <label className="flex flex-col gap-1">
                    <span className="text-xs font-medium text-muted-foreground">Folder</span>
                    <span className="truncate rounded-md border border-input bg-muted/40 px-2 py-1.5 text-sm text-foreground">
                      {path}
                    </span>
                  </label>
                )}

                <label className="flex flex-col gap-1">
                  <span className="text-xs font-medium text-muted-foreground">Name</span>
                  <Input
                    autoFocus
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                    aria-label={isWorkspaceEdit ? "Workspace name" : "Project name"}
                    className={cn(
                      "rounded-md border border-input bg-background px-2 py-1.5 text-sm text-foreground outline-none",
                      "focus-visible:ring-2 focus-visible:ring-ring",
                    )}
                  />
                </label>

                {/* PROJECT-ONLY. `resume_agent_sessions` is a column of `projects`, and the
                    handler above bails out on a dialog carrying no `projectId` — which is every
                    WORKSPACE rename. Without this gate the rename dialog (the same `edit` shell,
                    `entity="workspace"`) rendered the switch anyway wherever the caller supplies
                    `onResumeAgentSessionsChange` (`<BaseLayout>` always does): a project setting
                    inside a workspace dialog, permanently off and inert whatever the user did to
                    it. */}
                {mode === "edit" && !isWorkspaceEdit && onResumeAgentSessionsChange && (
                  <label
                    htmlFor="resume-agent-sessions"
                    className="flex items-start justify-between gap-3"
                  >
                    <span className="flex flex-col gap-0.5">
                      <span className="text-sm font-medium text-foreground">
                        Resume agent sessions
                      </span>
                      <span className="text-xs text-muted-foreground">
                        On relaunch, resume this project's active Claude sessions (exact
                        <code className="mx-1">--resume</code>) instead of a bare shell. Off by
                        default.
                      </span>
                    </span>
                    <Switch
                      id="resume-agent-sessions"
                      aria-label="Resume agent sessions on relaunch"
                      checked={resumeAgentSessions ?? false}
                      onCheckedChange={(checked) => onResumeAgentSessionsChange(checked)}
                    />
                  </label>
                )}

                {error && (
                  <p role="alert" className="text-sm text-destructive">
                    {error}
                  </p>
                )}

                <div className="mt-1 flex justify-end gap-2">
                  <Button type="button" variant="outline" size="sm" onClick={onCancel}>
                    Cancel
                  </Button>
                  <Button type="submit" size="sm" loading={submitting} disabled={!canSubmit}>
                    {confirmLabel}
                  </Button>
                </div>
              </form>
            </>
          )}
        </DialogPopup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
