import { CircleHelpIcon, UnplugIcon } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

/**
 * `<LifecycleBadge>` — the DOUBTFUL post-crash lifecycle state a runtime row carries (v2.5.5
 * phase 3), rendered BESIDE the row's `<StatusDot>` (not in place of it). This is a SEPARATE
 * axis from the run-state dot: the dot paints the factual run state, this badge surfaces the
 * DOUBT the sidecar's crash reconcile raised (`crates/nyx-sidecar/src/runtime/recovery.rs`):
 *
 *   - `interrupted` — a command instance persisted `running` whose backing process died with the
 *     old sidecar and cannot be re-verified (`running -> interrupted`). It is NOT auto-relaunched
 *     (the PRD non-goal — no blind resume); the user chooses. Surfaced via the crash reconcile's
 *     `interrupted` set, carried on the `workspace_restore_plan.interrupted` read model.
 *   - `unknown` — an agent session that was `active` at the crash and is now of UNCONFIRMED
 *     liveness (`active -> unknown`). Still a resume candidate, but doubtful. Read straight off
 *     the session's `state` in the live `workspace_runtime_snapshot`.
 *
 * Presentation only: every colour/size is a design-system token via the shared `<Badge>` pill —
 * no raw hex, no magic px. `interrupted` (a run that was cut) reads as a `warning`; `unknown`
 * (merely unconfirmed) as a quieter `muted` doubt so the two states stay distinguishable.
 */
export type LifecycleState = "interrupted" | "unknown";

const LIFECYCLE_META: Record<
  LifecycleState,
  { label: string; variant: "warning" | "muted"; Icon: typeof UnplugIcon }
> = {
  interrupted: { label: "Interrupted", variant: "warning", Icon: UnplugIcon },
  unknown: { label: "Unknown", variant: "muted", Icon: CircleHelpIcon },
};

export function LifecycleBadge({
  state,
  className,
}: {
  state: LifecycleState;
  className?: string;
}) {
  const { label, variant, Icon } = LIFECYCLE_META[state];
  return (
    <Badge
      variant={variant}
      data-lifecycle={state}
      title={label}
      className={cn("shrink-0", className)}
    >
      <Icon aria-hidden />
      {label}
    </Badge>
  );
}
