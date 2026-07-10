import { MessageSquareIcon } from "lucide-react";

import type { SpecDetail } from "../types";

/**
 * `<ReviewTab>` — the Spec's Review tab. In v2.4 this is a deliberate PLACEHOLDER / anchor
 * point: it is the FOYER where the already-developed review capability (branch `v2.4-review`)
 * GRAFTS in v2.5. The review content (diff, findings, comment threads) is explicitly OUT of
 * this phase's scope — this tab reserves the tab slot + navigation home so the graft has a
 * stable seat, and tells the user the review lands here.
 *
 * Data-model wise the review is a DERIVED view anchored on the Spec's (workspace, repos)
 * context (PDR-0015), which is exactly the phase-3 `detail.workspace_context` this detail
 * already carries — so the graft has its context ready without a new seam.
 */
export function ReviewTab(_props: { detail: SpecDetail }) {
  return (
    <div className="mx-auto flex max-w-xl flex-col items-center gap-3 py-16 text-center">
      <span className="grid size-12 place-items-center rounded-full bg-muted text-muted-foreground">
        <MessageSquareIcon className="size-5" />
      </span>
      <h2 className="font-heading text-base font-semibold text-foreground">Review</h2>
      <p className="text-sm text-muted-foreground">
        The review of this Spec — its diff, findings and comment threads — grafts here in a later
        iteration. This is its home tab.
      </p>
    </div>
  );
}
