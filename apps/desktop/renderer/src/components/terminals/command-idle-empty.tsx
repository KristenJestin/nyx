import { cn } from "@/lib/utils";
import type { RunState } from "./status-dot";
import type { WorkspaceRuntimeService } from "./use-workspace-runtime";

/**
 * `<CommandIdleEmpty>` — the EMPTY-STATE of a managed command's pane. When a command is idle
 * (never run this session, or reset between runs) there is no output to stream, so instead of a
 * bare black pane we render the TERMINAL surface with ONE tiny, discrete state label centred in
 * it — nothing else.
 *
 * This is the MINIMAL empty state the user validated (finding 48): the earlier summary CARD
 * (name + command + last-run + hint) was rejected. No card, no block, no big black panel — just
 * the command's current run-state word (`idle`) as a faint, small, centred label over the same
 * terminal-coloured background the running/settled output uses. Once the command runs, the
 * output panel takes over and this disappears.
 *
 * Presentation only — every value is a design-system token / the token-backed scale (no raw hex,
 * no magic px). English copy only (the label is the raw {@link RunState} word).
 */
export function CommandIdleEmpty({
  service,
  className,
}: {
  service: Pick<WorkspaceRuntimeService, "run_state">;
  className?: string;
}) {
  const state = service.run_state as RunState;
  return (
    <div className={cn("flex h-full w-full items-center justify-center bg-background", className)}>
      <span className="text-muted-foreground/50 text-xs lowercase tracking-wide select-none">
        {state}
      </span>
    </div>
  );
}
