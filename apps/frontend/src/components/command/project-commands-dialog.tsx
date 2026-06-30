import { useCallback, useState } from "react";

import { Button } from "@/components/ui/button";
import { Dialog, DialogBackdrop, DialogPopup } from "@/components/ui/dialog";
import { CommandsSection } from "./commands-section";

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

/**
 * `<ProjectCommandsDialog>` — the standalone "Project commands" modal. Its body is
 * now the shared [`CommandsSection`] (the validated `tabs` layout: a Commands tab
 * with inline create/edit IN PLACE, and an Import-from-package.json tab); this
 * component supplies only the modal CHROME (title + a neutral Close footer).
 *
 * The SAME `<CommandsSection>` is reused as the "Commands" pane of the
 * project-settings modal ([`ProjectSettingsDialog`]) — extracting it keeps both
 * surfaces on one implementation with no behavioural drift.
 */
export function ProjectCommandsDialog({
  open,
  projectId,
  projectName,
  importWorkspaceId,
  workspacePath,
  onClose,
}: ProjectCommandsDialogProps) {
  // Bump on each close to REMOUNT the body so its transient UI (an open create/edit
  // form, the active tab) is reset in the SAME commit as the close — a reopen starts
  // clean without an intermediate stale-state render, regardless of whether the
  // parent flips `open` (it does in the app; a controlled test may keep it true).
  const [bodyKey, setBodyKey] = useState(0);
  const handleClose = useCallback(() => {
    setBodyKey((k) => k + 1);
    onClose();
  }, [onClose]);

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

          {/* Body: the shared commands section (scrolls within the popup). Keyed so a
              close remounts it (transient UI reset in the same commit). */}
          <div className="min-h-0 flex-1 overflow-y-auto px-5">
            <CommandsSection
              key={bodyKey}
              active={open}
              projectId={projectId}
              importWorkspaceId={importWorkspaceId}
              workspacePath={workspacePath}
            />
          </div>

          {/* Footer: neutral Close. */}
          <div className="flex items-center justify-end gap-2 border-t border-border bg-muted/40 px-5 py-3.5">
            <Dialog.Close
              render={
                <Button type="button" variant="outline" size="sm" onClick={handleClose}>
                  Close
                </Button>
              }
            />
          </div>
        </DialogPopup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
