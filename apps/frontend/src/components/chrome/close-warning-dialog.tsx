import { Button } from "@/components/ui/button";
import { Dialog, DialogBackdrop, DialogPopup } from "@/components/ui/dialog";

import type { CloseWarning } from "./close-warning";

export interface CloseWarningDialogProps {
  /** Whether the dialog is shown (controlled). */
  open: boolean;
  /** The live agent sessions a close would drop (one line each). */
  warnings: CloseWarning[];
  /** Confirm: close anyway (drop the listed sessions). */
  onConfirm: () => void;
  /** Dismiss: keep the window open (cancel / backdrop / Escape). */
  onCancel: () => void;
}

/**
 * `<CloseWarningDialog>` — the confirm-before-close prompt for live agent sessions
 * (PRD-5 #6). Shown only when the backend reports active sessions whose project does
 * NOT auto-resume; it lists each (agent + terminal + workspace) so the user knows what
 * they'd lose, and requires an explicit "Close anyway" to proceed (Cancel keeps the
 * window open).
 *
 * Built on the shared ANIMATED `Dialog` primitives (Base UI + Motion, reduced-motion
 * aware), styled with the design-system tokens like the other dialogs. No new
 * component library — Base UI parts + the in-house `Button`.
 */
export function CloseWarningDialog({
  open,
  warnings,
  onConfirm,
  onCancel,
}: CloseWarningDialogProps) {
  const count = warnings.length;
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
          <Dialog.Title className="text-base font-semibold">
            {count === 1 ? "An agent session is still active" : "Agent sessions are still active"}
          </Dialog.Title>
          <Dialog.Description className="mt-1 text-sm text-muted-foreground">
            Closing now will drop {count === 1 ? "this session" : "these sessions"} — this
            project doesn't resume agent sessions on relaunch.
          </Dialog.Description>

          <ul className="mt-3 flex flex-col gap-1.5">
            {warnings.map((w, i) => (
              <li
                // A terminal can host more than one live session row (e.g. two agent
                // kinds), so `terminal_id` alone is not unique; key on the agent kind too
                // (and the index as a final tiebreaker) to avoid duplicate React keys.
                key={`${w.terminal_id}:${w.agent_kind}:${i}`}
                className="rounded-md border border-border bg-muted/40 px-2 py-1.5 text-sm text-foreground"
              >
                {w.message}
              </li>
            ))}
          </ul>

          <div className="mt-4 flex justify-end gap-2">
            <Button type="button" variant="outline" size="sm" onClick={onCancel}>
              Keep open
            </Button>
            <Button type="button" variant="destructive" size="sm" onClick={onConfirm}>
              Close anyway
            </Button>
          </div>
        </DialogPopup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
