import { useState } from "react";
import { Input } from "@base-ui/react/input";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Dialog, DialogBackdrop, DialogPopup } from "@/components/ui/dialog";

export interface AddWorkspaceDialogProps {
  /** Whether the dialog is shown (controlled). */
  open: boolean;
  /** Project the workspace will be added to (its name labels the dialog). */
  projectName: string;
  /** The picked folder path (shown read-only; the backend normalizes it). */
  path: string;
  /** Default workspace name (the folder basename); the user can edit it. */
  defaultName: string;
  /** A submission error to surface (e.g. duplicate path in this project). */
  error?: string | null;
  /** True while the create command is in flight. */
  submitting?: boolean;
  /** Confirm with the (possibly edited) name. */
  onConfirm: (name: string) => void;
  /** Dismiss without creating (cancel / backdrop / Escape). */
  onCancel: () => void;
}

/**
 * `<AddWorkspaceDialog>` — the name-edit step of the add-workspace flow (ZDZ).
 * After the folder picker returns a path, this Base UI dialog lets the user
 * confirm/edit the workspace NAME (pre-filled with the folder basename) before
 * `create_workspace` runs. The picked path is shown read-only — stored paths are
 * normalized by the backend. A backend duplicate-path rejection (same path in
 * the same project) is surfaced inline via `error`.
 *
 * Built on Base UI's `Dialog` + `Input`, styled with the design-system tokens
 * (the popover/border/ring/foreground variables), matching `button.tsx`'s
 * shadcn-like style.
 */
export function AddWorkspaceDialog({
  open,
  projectName,
  path,
  defaultName,
  error,
  submitting = false,
  onConfirm,
  onCancel,
}: AddWorkspaceDialogProps) {
  // The editable name is initialized from `defaultName`. A fresh folder pick
  // remounts this dialog (it's keyed on the picked path in `useManualAdd`), so
  // each reopen starts from the correct default with no effect-driven reset.
  const [name, setName] = useState(defaultName);

  const trimmed = name.trim();
  const canSubmit = trimmed.length > 0 && !submitting;

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
          <Dialog.Title className="text-base font-semibold">Add workspace</Dialog.Title>
          <Dialog.Description className="mt-1 text-sm text-muted-foreground">
            Add a folder as a workspace in{" "}
            <span className="font-medium text-foreground">{projectName}</span>.
          </Dialog.Description>

          <form
            onSubmit={(e) => {
              e.preventDefault();
              if (canSubmit) onConfirm(trimmed);
            }}
            className="mt-4 flex flex-col gap-3"
          >
            <label className="flex flex-col gap-1">
              <span className="text-xs font-medium text-muted-foreground">Folder</span>
              <span className="truncate rounded-md border border-input bg-muted/40 px-2 py-1.5 text-sm text-foreground">
                {path}
              </span>
            </label>

            <label className="flex flex-col gap-1">
              <span className="text-xs font-medium text-muted-foreground">Name</span>
              <Input
                autoFocus
                value={name}
                onChange={(e) => setName(e.target.value)}
                aria-label="Workspace name"
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
                Add workspace
              </Button>
            </div>
          </form>
        </DialogPopup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
