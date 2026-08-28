import { cn } from "@/lib/utils";
import {
  commandRunPhase,
  commandRunPhaseLabel,
  useCommandRanThisSession,
} from "./command-run-phase";
import type { WorkspaceRuntimeService } from "@/components/runtime/use-workspace-runtime";

/**
 * `<CommandRunEmpty>` — the EMPTY-STATE of a managed command's pane: what shows when there is no
 * output to stream. Instead of a bare black pane, the TERMINAL surface carries one tiny, discrete
 * line centred in it — nothing else.
 *
 * This is the MINIMAL empty state the user validated (finding 48): the earlier summary CARD
 * (name + command + last-run + hint) was rejected. No card, no block, no big black panel.
 *
 * ## What changed, and why (review 01M0ABR6V501D0SPR2BYBVZC4A, minor)
 *
 * It used to render the RAW runtime word — `idle` — as its only content, which a user reads as
 * "inactive" while the command had in fact never run. It now names the PHASE
 * ({@link commandRunPhase}), which tells "never started" from "stopped" and from a finished run,
 * and for a command that has not run it points at the trigger rather than reporting a state: the
 * command's NAME plus how to launch it. The runtime's own `idle` vocabulary is untouched — see the
 * header of `command-run-phase.ts`.
 *
 * Presentation only — every value is a design-system token / the token-backed scale (no raw hex,
 * no magic px). English copy only.
 */
export function CommandRunEmpty({
  service,
  className,
}: {
  service: Pick<WorkspaceRuntimeService, "id" | "name" | "run_state" | "exit_code">;
  className?: string;
}) {
  const ranThisSession = useCommandRanThisSession(service.id, service.run_state === "running");
  const phase = commandRunPhase({ state: service.run_state, ranThisSession });
  // The EXIT CODE goes in. `commandRunPhaseLabel` has always been able to say
  // "Finished with an error — exit N", but the only production call passed no options, so that
  // half of the sentence had no display path at all (review 01M0ASWCH977S4J4XCC0B8N89N).
  const label = commandRunPhaseLabel(phase, { exitCode: service.exit_code });
  return (
    <div className={cn("flex h-full w-full items-center justify-center bg-background", className)}>
      {phase === "never-run" ? (
        // THE INVITATION, not a state report: what this command is, and what makes it go.
        <span className="flex items-baseline gap-1.5 text-xs select-none">
          <span className="font-mono text-muted-foreground">{service.name}</span>
          <span className="text-muted-foreground/50">— press Start to run it</span>
        </span>
      ) : (
        <span className="text-muted-foreground/50 text-xs tracking-wide select-none">{label}</span>
      )}
    </div>
  );
}
