import { useEffect, useRef } from "react";

import { nyxBridge } from "@/bridge";
import type { ResumePark, WorkspaceRestorePlan } from "@/bridge";

/**
 * `use-lazy-resume` — the RENDERER half of the LAZY agent-session resume (v2.5.5). It CONSUMES the
 * `workspace_restore_plan` read model (declared in the contract but, until now, consumed by no
 * component) for each workspace of the ENTERED project and PUSHES the CONFIDENT resumable sessions
 * to the sidecar (`set_resume_parks`), which parks each `claude --resume <id>` and injects it
 * exactly once at that terminal's PTY spawn — or immediately if the terminal's PTY is already live.
 *
 * ## Confident vs doubtful — the PARTITION with the restore panel
 * This hook auto-drains ONLY the CONFIDENT resumable sessions (`resume === true` AND
 * `uncertain === false`): a session whose candidate state survived as `active` (a clean shutdown,
 * no crash reconcile). The DOUBTFUL resumable sessions (`uncertain === true` — a post-crash
 * `unknown`/stale candidate) are DELIBERATELY left untouched here and handed instead to the
 * `<WorkspaceRestorePanel>` (`use-restore-plan`), which surfaces them for the user to Resume or
 * Ignore. The two consumers thus partition `resume_sessions` on `uncertain` and never overlap: a
 * doubtful session is never auto-parked by lazy, so an Ignore in the panel is truly blocking (no
 * park is ever set for it → nothing is injected at PTY spawn).
 *
 * ## Why here (lazy, not eager-at-boot)
 * The posture is LAZY: resume happens when you OPEN a workspace's terminals, not blindly at boot.
 * Entering a project mounts its workspaces' runtime and (via the deck) their terminals; this hook,
 * mounted on that page, reads each workspace's restore plan and hands the sidecar the resume lines
 * to inject. `workspace_restore_plan` REUSES the existing `decide_resume` / `workspace_resume_plan`
 * policy (no third resume system) — so `resume === true` entries already honour the project toggle,
 * the candidate state, the transcript-on-disk probe and the shell target; a toggle-OFF or a
 * non-candidate simply yields no resumable entry, hence no injection.
 *
 * ## Idempotence
 * Each workspace's plan is pushed AT MOST ONCE per app run (a ref-guarded set), and the SIDECAR
 * additionally guards each terminal record one-shot — so a re-render, a re-entered project, or a
 * re-pushed plan never injects a resume twice. An injection whose PTY write fails marks its session
 * `resume_failed` on the sidecar (a clean `resume-failed` state, no retry loop). Best-effort: a
 * failed plan read simply means no resume this open (a relaunch re-scans) and never breaks the page.
 */
export function useLazyResume(workspaceIds: readonly string[]): void {
  // Per-app-run guard: a workspace's restore plan is pushed at most once. The sidecar also guards
  // each record id one-shot, so a re-push would be idempotent anyway — this just avoids a redundant
  // round-trip. A ref (not state) so marking a workspace pushed never itself triggers a re-render.
  const pushedRef = useRef<Set<string>>(new Set());

  useEffect(() => {
    // The `pushedRef` guard already guarantees each workspace's plan is pushed EXACTLY ONCE for
    // the app's lifetime — including across React StrictMode's dev-only mount→unmount→remount,
    // since refs survive it. So we must NOT discard a resolved invoke on the StrictMode cleanup: a
    // prior version set a `cancelled` flag in cleanup, which (StrictMode: run1 marks the ref +
    // starts the async → cleanup cancels run1 → run2 skips the already-marked workspace → run1's
    // resolved plan bailed on `if (cancelled) return`) meant `set_resume_parks` was NEVER called —
    // no resume in `tauri dev` (the bug only shows in dev; prod StrictMode is inert). Marking the
    // ref BEFORE the await keeps the push idempotent (run2 finds the workspace already marked, so
    // it never launches a second invoke), and the sidecar's one-shot `attempted` guard is the
    // final backstop against a double injection — so no cleanup-time cancellation is needed.
    for (const workspaceId of workspaceIds) {
      if (pushedRef.current.has(workspaceId)) continue;
      pushedRef.current.add(workspaceId);
      void (async () => {
        try {
          const plan = await nyxBridge.invoke<WorkspaceRestorePlan>("workspace_restore_plan", {
            workspaceId,
          });
          const parks: ResumePark[] = (plan.resume_sessions ?? [])
            // Auto-drain the CONFIDENT resumable sessions only: `resume === true` AND NOT
            // `uncertain`. The doubtful (`uncertain`) ones belong to the restore panel
            // (`use-restore-plan`), never to this silent auto-resume — the partition that makes
            // the panel's Ignore actually block a post-crash session.
            .filter((entry) => entry.resume && entry.resume_command != null && !entry.uncertain)
            .map((entry) => ({
              terminalId: entry.terminal_id,
              sessionId: entry.session_id,
              command: entry.resume_command as string,
            }));
          if (parks.length === 0) return;
          await nyxBridge.invoke("set_resume_parks", { parks });
        } catch {
          // Best-effort: a failed restore-plan read / park push means no lazy resume for this
          // workspace this open; a relaunch re-scans. Never surface an error to the page.
        }
      })();
    }
  }, [workspaceIds]);
}
