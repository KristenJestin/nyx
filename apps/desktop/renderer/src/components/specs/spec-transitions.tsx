import { useState } from "react";

import { Button } from "@/components/ui/button";
import { Dialog, DialogBackdrop, DialogPopup } from "@/components/ui/dialog";

import type { SpecDetail, SpecStatus } from "./types";
import { useSpecTransition, type SpecTransitionTarget } from "./use-spec-transition";

/**
 * THE LIFECYCLE EDGES, read off the ONE guard that pronounces them:
 * `crates/nyx-core/src/db.rs`, `SpecTransition::source_allowed` (~l. 2782). Each entry is the set
 * of targets LEGAL from that status — nothing here is invented, and no edge the backend would
 * refuse is offered:
 *
 *   * `Ready`  : `draft → ready`
 *   * `Start`  : `ready → in_progress`
 *   * `Review` : `in_progress → in_review`
 *   * `Done`   : `in_review → completed`
 *   * `Cancel` : any NON-terminal source (`draft` / `ready` / `in_progress` / `in_review`)
 *                `→ cancelled`
 *
 * The four statuses that offer NOTHING are spelled out rather than left to a lookup miss, because
 * each is a decision: `completed` / `cancelled` / `archived` are TERMINAL, and `blocked` is
 * deliberately absent from `Cancel`'s source list — the guard does not accept it, so this row
 * cannot pretend otherwise. There is no edge BACK to `draft` in the guard either, so no button
 * offers one.
 */
export const LEGAL_TRANSITIONS: Record<SpecStatus, readonly SpecTransitionTarget[]> = {
  draft: ["ready", "cancelled"],
  ready: ["in_progress", "cancelled"],
  in_progress: ["in_review", "cancelled"],
  in_review: ["completed", "cancelled"],
  blocked: [],
  completed: [],
  cancelled: [],
  archived: [],
};

/**
 * The targets that go through an explicit CONFIRMATION DIALOG before anything is sent. They are the
 * moves a human should not make by brushing past a button: `ready` submits the design to the
 * pre-ready validations and opens it to work, `completed` closes it, `cancelled` abandons it.
 * `in_progress` and `in_review` are ordinary steps forward and fire straight off their button.
 *
 * These three ARE the backend's sensitive set (`db::SpecTransition::is_sensitive`, mirrored by
 * `SERVER_SENSITIVE` in `use-spec-transition.ts`): the dialog is what makes `userConfirmed` true,
 * so the flag is never sent on a gesture nobody was asked about.
 */
const NEEDS_DIALOG = ["ready", "completed", "cancelled"] as const;

/** How a target reads AS A BUTTON in the row: its label and its weight. */
interface RowMeta {
  readonly label: string;
  /** The forward move leads (`default`); the abandonment stays quiet (`outline`). */
  readonly variant: "default" | "outline";
}

const ROW_META: Record<SpecTransitionTarget, RowMeta> = {
  ready: { label: "Mark ready", variant: "default" },
  in_progress: { label: "Start work", variant: "default" },
  in_review: { label: "Send to review", variant: "default" },
  completed: { label: "Mark done", variant: "default" },
  cancelled: { label: "Cancel Spec", variant: "outline" },
};

/** What the confirmation dialog SAYS for a target. Only the {@link NEEDS_DIALOG} three have one. */
interface ConfirmMeta {
  readonly title: string;
  /** What the move actually does — the sentence the human is confirming. */
  readonly description: string;
  /** The dismissal label ("Cancel" reads as an outcome on the cancellation dialog, so it varies). */
  readonly dismiss: string;
  readonly confirm: string;
  /** `destructive` for the abandonment, `default` for the two forward moves. */
  readonly confirmVariant: "default" | "destructive";
}

const CONFIRM_META: Record<(typeof NEEDS_DIALOG)[number], ConfirmMeta> = {
  ready: {
    title: "Mark this Spec ready?",
    description:
      "The design is submitted to the pre-ready validations and the Spec becomes workable. A Spec whose design is incomplete is refused, and the reason is shown here.",
    dismiss: "Cancel",
    confirm: "Mark ready",
    confirmVariant: "default",
  },
  completed: {
    title: "Mark this Spec done?",
    description:
      "The Spec is closed. A Spec that still has work open is refused, and the reason is shown here.",
    dismiss: "Cancel",
    confirm: "Mark done",
    confirmVariant: "default",
  },
  cancelled: {
    title: "Cancel this Spec?",
    description:
      "The Spec is abandoned and lands in a terminal state — it can no longer be moved through its lifecycle.",
    dismiss: "Keep the Spec",
    confirm: "Cancel the Spec",
    confirmVariant: "destructive",
  },
};

/** Does this target ask the human first? Narrows to the {@link CONFIRM_META} keys. */
function needsDialog(to: SpecTransitionTarget): to is (typeof NEEDS_DIALOG)[number] {
  return (NEEDS_DIALOG as readonly SpecTransitionTarget[]).includes(to);
}

/**
 * `<SpecTransitions>` — the Spec detail's LIFECYCLE CONTROLS: one button per transition that is
 * LEGAL from the Spec's current status, and nothing else.
 *
 * Until now nothing in the app could move a Spec: `spec_transition` existed in the core and was
 * routed nowhere, so the state machine only ever advanced through an agent's MCP call. With the
 * orchestration engine retired (PDR-0031) the Spec state machine is the only one left, and the
 * human needs a handle on it — this is it.
 *
 * # What it offers is what the guard allows
 *
 * The offered set comes from {@link LEGAL_TRANSITIONS}, transcribed from
 * `db::SpecTransition::source_allowed`. A terminal Spec (`completed` / `cancelled` / `archived`) —
 * and a `blocked` one, which the guard does not accept as a source for anything — renders NO row
 * at all, not a row of disabled buttons: an affordance that can never fire is noise.
 *
 * # The three moves that ask first
 *
 * `ready`, `completed` and `cancelled` open the shared confirmation dialog and are only sent once
 * it is confirmed ({@link NEEDS_DIALOG}). The dialog is the app's own — the same `Dialog.Root` +
 * `DialogBackdrop` + `DialogPopup` composition as `<CloseWarningDialog>` — so the two read alike
 * and neither hand-rolls a modal.
 *
 * # A refusal is never swallowed
 *
 * The backend refuses a `ready` whose pre-ready validations failed, and a `done` whose work is
 * still open, with a sentence that says WHICH. That sentence is shown in a live region under the
 * row (and as a toast, from {@link useSpecTransition}) — the human has to be able to read it.
 */
export function SpecTransitions({
  detail,
  refresh,
}: {
  detail: SpecDetail;
  /** Re-pull the Spec detail after a move — the existing `spec_detail` channel, never a patch. */
  refresh: () => Promise<void>;
}) {
  const [confirming, setConfirming] = useState<(typeof NEEDS_DIALOG)[number] | null>(null);
  const actions = useSpecTransition();

  const { spec } = detail;
  // An unknown wire status offers nothing rather than throwing — same leniency as `statusMeta`.
  const targets = LEGAL_TRANSITIONS[spec.status as SpecStatus] ?? [];
  if (targets.length === 0) return null;

  const run = async (to: SpecTransitionTarget) => {
    setConfirming(null);
    const moved = await actions.transition({
      specId: spec.id,
      to,
      // The Spec's OWN anchor, when it already carries one: `start` poses the anchor in the same
      // write, and handing back the id the Spec is already bound to keeps that binding. With no
      // anchor yet the argument is omitted and the backend resolves one.
      workspaceId: to === "in_progress" ? spec.workspace_id : null,
    });
    if (moved !== null) await refresh();
  };

  const press = (to: SpecTransitionTarget) => {
    actions.clearError();
    if (needsDialog(to)) {
      setConfirming(to);
      return;
    }
    void run(to);
  };

  const pendingMeta = confirming === null ? null : CONFIRM_META[confirming];

  return (
    <div className="flex min-w-0 flex-col items-end gap-1.5">
      <div className="flex items-center gap-2" role="group" aria-label="Spec lifecycle">
        {targets.map((to) => {
          const meta = ROW_META[to];
          return (
            <Button
              key={to}
              type="button"
              size="sm"
              variant={meta.variant}
              disabled={actions.pending}
              loading={actions.target === to}
              onClick={() => press(to)}
            >
              {meta.label}
            </Button>
          );
        })}
      </div>

      {actions.error !== null ? (
        <p role="alert" className="max-w-xs text-right text-2xs text-destructive">
          {actions.error}
        </p>
      ) : null}

      <Dialog.Root
        open={confirming !== null}
        onOpenChange={(next) => {
          if (!next) setConfirming(null);
        }}
      >
        <Dialog.Portal>
          <DialogBackdrop />
          <DialogPopup>
            <Dialog.Title className="text-base font-semibold">
              {pendingMeta?.title ?? ""}
            </Dialog.Title>
            <Dialog.Description className="mt-1 text-sm text-muted-foreground">
              {pendingMeta?.description ?? ""}
            </Dialog.Description>

            <div className="mt-4 flex justify-end gap-2">
              <Button type="button" variant="outline" size="sm" onClick={() => setConfirming(null)}>
                {pendingMeta?.dismiss ?? "Cancel"}
              </Button>
              <Button
                type="button"
                variant={pendingMeta?.confirmVariant ?? "default"}
                size="sm"
                disabled={actions.pending}
                onClick={() => {
                  if (confirming !== null) void run(confirming);
                }}
              >
                {pendingMeta?.confirm ?? "Confirm"}
              </Button>
            </div>
          </DialogPopup>
        </Dialog.Portal>
      </Dialog.Root>
    </div>
  );
}
