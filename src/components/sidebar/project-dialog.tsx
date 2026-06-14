import { useState } from "react";
import { Input } from "@base-ui/react/input";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Dialog, DialogBackdrop, DialogPopup } from "@/components/ui/dialog";

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
  /** A submission error to surface inline. */
  error?: string | null;
  /** True while the create/edit/delete command is in flight. */
  submitting?: boolean;
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
 *    its workspaces are removed but its terminals survive (become loose), and
 *    requires an explicit "Delete project" click.
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
  error,
  submitting = false,
  onConfirm,
  onCancel,
}: ProjectDialogProps) {
  // Editable name, initialized from `defaultName`. The dialog is remounted per
  // flow (keyed in the caller) so each open starts from the right default.
  const [name, setName] = useState(defaultName);

  const trimmed = name.trim();
  const isDelete = mode === "delete";
  const canSubmit = isDelete ? !submitting : trimmed.length > 0 && !submitting;

  const title =
    mode === "create" ? "Add project" : mode === "edit" ? "Rename project" : "Delete project";
  const confirmLabel = mode === "create" ? "Add project" : mode === "edit" ? "Save" : "Delete";

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
                Delete <span className="font-medium text-foreground">{defaultName}</span> and its
                workspaces. Any open terminals are kept (they just become loose, unattached
                terminals) — nothing is closed.
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
                    aria-label="Project name"
                    className={cn(
                      "rounded-md border border-input bg-background px-2 py-1.5 text-sm text-foreground outline-none",
                      "focus-visible:ring-2 focus-visible:ring-ring",
                    )}
                  />
                </label>

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
