import { RotateCcwIcon, SquareTerminalIcon, XIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/toast";
import { agentProviderFor } from "@/components/sidebar/agent-providers";
import { LifecycleBadge } from "./lifecycle-badge";
import { useRestorePlan, type RestorableSession } from "./use-restore-plan";

/**
 * `<WorkspaceRestorePanel>` — the post-crash RESTORATION UI (v2.5.5 phase 3). After a crash the
 * reconcile leaves a workspace's agent sessions doubtful (`active -> unknown`); this panel lists
 * the RESUMABLE ones (from the `workspace_restore_plan` read model, via `useRestorePlan`) and lets
 * the user explicitly choose, per session:
 *   - RESUME — re-inject that session's `claude --resume <id>` through the phase-2
 *     `set_resume_parks` mechanism (idempotent one-shot on the sidecar);
 *   - IGNORE — dismiss it WITHOUT resuming (nyx never blind-resumes — the user decides).
 *
 * It renders NOTHING when a workspace has no resumable interrupted session, so the tree stays
 * clean in the common (no-crash) case. Every colour/size is a design-system token; the resume/
 * ignore controls are the shared `<Button>`, the doubt marker the shared `<LifecycleBadge>`.
 */
export function WorkspaceRestorePanel({ workspaceId }: { workspaceId: string }) {
  const { restorablesForWorkspace, resume, ignore } = useRestorePlan();
  const sessions = restorablesForWorkspace(workspaceId);
  if (sessions.length === 0) return null;

  return (
    <section
      aria-label="Restore interrupted sessions"
      className="mx-2 mb-2 rounded-lg border border-warning/40 bg-warning/[0.06] p-2"
    >
      <header className="flex items-center gap-2 px-1 pb-1.5">
        <LifecycleBadge state="interrupted" />
        <p className="min-w-0 flex-1 text-2xs text-muted-foreground leading-snug">
          Interrupted after a crash — choose what to resume.
        </p>
      </header>
      <ul className="flex flex-col gap-1">
        {sessions.map((session) => (
          <RestoreRow
            key={session.sessionId}
            session={session}
            onResume={() => {
              resume(session);
              toast.info("Resuming agent session…");
            }}
            onIgnore={() => ignore(workspaceId, session.sessionId)}
          />
        ))}
      </ul>
    </section>
  );
}

/** One resumable session line: provider glyph + label + doubt badge + Resume / Ignore controls. */
function RestoreRow({
  session,
  onResume,
  onIgnore,
}: {
  session: RestorableSession;
  onResume: () => void;
  onIgnore: () => void;
}) {
  const provider = agentProviderFor(session.agentKind);
  const Icon = provider?.icon ?? SquareTerminalIcon;
  const label = provider?.label ?? session.agentKind;

  return (
    <li className="flex items-center gap-2 rounded-md bg-background/40 px-2 py-1.5 text-sm">
      <Icon className="size-4 shrink-0 text-muted-foreground" />
      <span className="min-w-0 flex-1 truncate text-foreground">{label}</span>
      {/* Every session the panel lists is a post-crash DOUBTFUL (`unknown` / stale) candidate — the
          confident ones are auto-drained by `use-lazy-resume` and never reach here — so each row
          carries the `unknown` doubt marker unconditionally. */}
      <LifecycleBadge state="unknown" />
      <Button size="xs" variant="secondary" onClick={onResume} aria-label="Resume session">
        <RotateCcwIcon />
        Resume
      </Button>
      <Button size="xs" variant="ghost" onClick={onIgnore} aria-label="Ignore session">
        <XIcon />
        Ignore
      </Button>
    </li>
  );
}
