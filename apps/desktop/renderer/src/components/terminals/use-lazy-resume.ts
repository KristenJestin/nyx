import { useEffect, useRef } from "react";

import { errorText, nyxBridge } from "@/bridge";
import type { ResumePark, WorkspaceRestorePlan, WorkspaceResumeEntry } from "@/bridge";

/**
 * The prefix EVERY lazy-resume trace line carries, so `[nyx-resume]` is the one string to grep in
 * the renderer console (DevTools / a CDP driver) when the question is "why did nothing resume?".
 * The sidecar half of the same story uses its own `[nyx-sidecar]` prefix on stderr — the two halves
 * are deliberately distinct because they answer different questions: this one says what the PLAN
 * decided, the sidecar's says what the INJECTION did with it.
 */
const RESUME_TRACE = "[nyx-resume]";

/**
 * `use-lazy-resume` — the RENDERER half of the LAZY agent-session resume (v2.5.5). It CONSUMES the
 * `workspace_restore_plan` read model for each workspace of the ENTERED project and PUSHES the
 * resumable sessions to the sidecar (`set_resume_parks`), which parks each `claude --resume <id>`
 * and injects it exactly once at that terminal's PTY spawn — or immediately if the terminal's PTY
 * is already live (the sidecar gates that inject on the CURRENT activity, #28: never into a live
 * working session).
 *
 * ## Silent resume of every resumable session (crash-UI reverted, v2.5.8)
 * This hook auto-drains EVERY resumable session (`resume === true`), whether its candidate state
 * is `active` or `unknown`. There is no longer a confident/doubtful partition: the v2.5.5
 * post-crash restore panel (which used to own the `uncertain` sessions) was reverted, so a crash
 * no longer produces `unknown` sessions and every resumable session simply resumes SILENTLY here,
 * gated only by the project resume toggle (`decide_resume`) and the sidecar's inject-time
 * process-liveness gate (never injected into a terminal that already hosts a live agent process).
 *
 * ## Why here (lazy, not eager-at-boot) — and what that means for the logs
 * The posture is LAZY: resume happens when you OPEN a workspace's terminals, not blindly at boot.
 * Entering a project mounts its workspaces' runtime and (via the deck) their terminals; this hook,
 * mounted on that page, reads each workspace's restore plan and hands the sidecar the resume lines
 * to inject. `workspace_restore_plan` REUSES the existing `decide_resume` / `workspace_resume_plan`
 * policy (no third resume system) — so `resume === true` entries already honour the project toggle,
 * the candidate state, the transcript-on-disk probe and the shell target; a toggle-OFF or a
 * non-candidate simply yields no resumable entry, hence no injection.
 *
 * CONSEQUENCE, and the false trail it has already cost: **the resume is NOT part of the boot
 * sequence**, so a startup log holds no resume trace and its absence there proves nothing. The
 * first `[nyx-resume]` line is written only once BOTH of these hold — never earlier, and if either
 * never holds, never at all:
 *   1. `<TerminalsPage>` is MOUNTED — it is the ONLY caller of this hook (`terminals-page.tsx`,
 *      the `/` index route). The app does open on `/` (`router.tsx` `createAppRouter` builds the
 *      memory history with `["/"]` and nothing persists a last route), so the page normally comes
 *      up shortly after launch; but it is a ROUTED page, so navigating to ANY other route unmounts
 *      it and no resume can be driven from there.
 *   2. a PROJECT is ENTERED and it has workspaces — `workspaceIds` is derived from the entered
 *      project (`terminals-page.tsx`), so it is EMPTY until `<BaseLayout>`'s last-project restore
 *      lands, and stays empty if no project is entered. While it is empty, no plan is read and
 *      nothing resumes — silently, and by design.
 *
 * ## Idempotence, and its real scope
 * A workspace's plan is pushed at most once PER MOUNT OF THIS HOOK — the guard is a `useRef` set,
 * so it lives exactly as long as the hook instance that owns it, NOT for the whole app run. And
 * that instance belongs to `<TerminalsPage>`, a ROUTED page under the always-mounted shell
 * (`<BaseLayout>`), not to the shell itself: every remount of the page starts from a FRESH set and
 * reads the plans again. That is safe because the SIDECAR additionally guards each terminal record
 * one-shot (`ResumeParks`), so a re-render, a re-entered project, or a re-pushed plan never injects
 * a resume twice. An injection whose PTY write fails marks its session `resume_failed` on the
 * sidecar (a clean `resume-failed` state, no retry loop).
 *
 * ## Best-effort, but never mute
 * A failed plan read or a failed park push NEVER reaches the page — no throw, no error state, no
 * UI. It is TRACED (`console.warn`) and the workspace is left UN-consumed so the next run of the
 * effect tries again. There is no timer and no retry loop: a failure simply stops being a permanent
 * one.
 */
export function useLazyResume(workspaceIds: readonly string[]): void {
  // Per-MOUNT guard: a workspace's restore plan is pushed at most once while this hook instance
  // lives. The sidecar also guards each record id one-shot, so a re-push would be idempotent
  // anyway — this just avoids a redundant round-trip. A ref (not state) so marking a workspace
  // pushed never itself triggers a re-render.
  const pushedRef = useRef<Set<string>>(new Set());

  useEffect(() => {
    // The `pushedRef` guard already guarantees each workspace's plan is pushed EXACTLY ONCE for
    // the lifetime of this hook instance — including across React StrictMode's dev-only
    // mount→unmount→remount, since refs survive it. So we must NOT discard a resolved invoke on
    // the StrictMode cleanup: a prior version set a `cancelled` flag in cleanup, which (StrictMode:
    // run1 marks the ref + starts the async → cleanup cancels run1 → run2 skips the already-marked
    // workspace → run1's resolved plan bailed on `if (cancelled) return`) meant `set_resume_parks`
    // was NEVER called — no resume in dev (the bug only shows in dev; prod StrictMode is inert).
    // Marking the ref BEFORE the await keeps the push idempotent (run2 finds the workspace already
    // marked, so it never launches a second invoke), and the sidecar's one-shot `attempted` guard
    // is the final backstop against a double injection — so no cleanup-time cancellation is needed.
    //
    // The mark is a CLAIM, not a receipt: it is RELEASED below when the attempt did not go
    // through. A transient failure at the first mount used to consume the workspace for good and
    // kill every later resume, silently — that is the "ça resume jamais" this release repairs. The
    // claim-then-release order is what lets BOTH properties hold at once: StrictMode's second run
    // still finds the workspace claimed (no double invoke), and a failed attempt still leaves it
    // free for the next run of this effect.
    //
    // WHAT THE RELEASE DOES NOT DO IS TRIGGER THAT RUN. This effect depends on `workspaceIds`
    // alone (memoised on the workspace list in `terminals-page.tsx`), so a released workspace is
    // retried when that list changes or when this page is re-mounted — NOT on a timer, and NOT
    // right away. Deliberate: an immediate re-run would spin as fast as the invoke rejects the
    // whole time the sidecar is down, which is the commonest failure this branch sees. The traces
    // below say exactly that rather than promising a retry with no clock behind it.
    for (const workspaceId of workspaceIds) {
      if (pushedRef.current.has(workspaceId)) continue;
      pushedRef.current.add(workspaceId);
      void (async () => {
        const completed = await attemptWorkspaceResume(workspaceId);
        if (!completed) pushedRef.current.delete(workspaceId);
      })();
    }
  }, [workspaceIds]);
}

/**
 * Run ONE lazy-resume attempt for `workspaceId`: read its restore plan, PUBLISH the verdict of
 * every candidate session, and push the resumable ones to the sidecar.
 *
 * Returns whether the attempt WENT THROUGH — `true` when the plan was read and the resumable
 * sessions were parked (including the legitimate "there was nothing to park" outcome), `false`
 * when the plan read or the park push FAILED. Only a `true` consumes the workspace; a `false`
 * hands it back to the caller so the NEXT run of the effect can try again. There is no timer and
 * no self-retry here — re-tryable is not retried, and the next run of the effect is the workspace
 * list changing or this page re-mounting, never a clock.
 *
 * NEVER rejects: the single `catch` spans the whole body, so the page can never see a resume
 * error. But best-effort is not mute — each of the three exits writes a line, and the plan-READ
 * failure and the park-PUSH failure write DIFFERENT ones, because they are different pannes (the
 * plan never came back vs. the batch was not accepted).
 */
async function attemptWorkspaceResume(workspaceId: string): Promise<boolean> {
  // What is being attempted RIGHT NOW, so the failure line below names the real fault instead of a
  // generic "resume failed". Reassigned once, immediately before the push.
  let attempting = "reading the restore plan";
  try {
    const plan = await nyxBridge.invoke<WorkspaceRestorePlan>("workspace_restore_plan", {
      workspaceId,
    });
    const entries = plan?.resume_sessions ?? [];
    // PUBLISH THE VERDICT. `decide_resume` builds a `ResumeSkipReason` on every non-resume path
    // "so the caller can log/observe" and `skip_reason_tag` gives it a stable label that travels
    // in `resume_sessions[].skip_reason` — this hook used to be the caller that threw it away.
    for (const entry of entries) traceResumeVerdict(workspaceId, entry);
    const parks: ResumePark[] = entries
      // Auto-drain EVERY resumable session (`resume === true` with a command). The
      // v2.5.5 confident/doubtful partition is gone (crash-UI reverted, v2.5.8): a crash
      // is a clean shutdown, so there are no doubtful (`uncertain`) sessions to hold back —
      // everything resumable resumes silently (gated by the toggle + the #28 inject gate).
      .filter((entry) => entry.resume && entry.resume_command != null)
      .map((entry) => ({
        terminalId: entry.terminal_id,
        sessionId: entry.session_id,
        command: entry.resume_command as string,
      }));
    if (parks.length === 0) {
      // NOTHING TO RESUME — a legitimate, complete outcome, not a panne. It gets its own line
      // precisely so it stops being indistinguishable from a failure: the plan was read, it just
      // held no resumable entry (an empty workspace, or every candidate skipped — each skip's own
      // reason was traced just above).
      console.info(
        `${RESUME_TRACE} workspace ${workspaceId}: NOTHING TO RESUME — ` +
          `${entries.length} candidate session(s) in the plan, none resumable.`,
      );
      return true;
    }
    attempting = `pushing ${parks.length} resume park(s)`;
    await nyxBridge.invoke("set_resume_parks", { parks });
    console.info(
      `${RESUME_TRACE} workspace ${workspaceId}: pushed ${parks.length} resume park(s) to the ` +
        `sidecar; each is injected once at its terminal's PTY spawn.`,
    );
    return true;
  } catch (error) {
    console.warn(
      `${RESUME_TRACE} workspace ${workspaceId}: ${attempting} FAILED — ${errorText(error)}. ` +
        `Nothing is retried on a timer: the workspace is un-claimed and tried again the next time ` +
          `the resume effect runs — i.e. when the project's workspace list changes, or when the ` +
          `Terminals page is re-mounted. Until then, no resume for it.`,
    );
    return false;
  }
}

/**
 * Write ONE candidate session's verdict: RESUME, or the SKIP reason the backend already decided.
 * The line names the WORKSPACE, the TERMINAL and the SESSION, without which an isolated verdict
 * cannot be tied back to anything on screen.
 *
 * The skip label is `entry.skip_reason` VERBATIM — the `skip_reason_tag` vocabulary owned by
 * `agent_resume.rs` (`option_off`, `closed_voluntarily`, `session_ended`, `already_resume_failed`,
 * `no_conversation`, `unsupported_target`, `no_resume_command`). This hook invents no reason of its
 * own and maps none: a tag it has never seen prints as itself. The one case with no tag to print is
 * an entry that claims `resume` with no command — impossible from `workspace_resume_plan`, which
 * pairs `resume: true` with `Some(command)` — and it is reported as the shape anomaly it is rather
 * than dressed up as an eighth reason.
 */
function traceResumeVerdict(workspaceId: string, entry: WorkspaceResumeEntry): void {
  const where =
    `workspace ${workspaceId} terminal ${entry.terminal_id} session ${entry.session_id} ` +
    `(${entry.agent_kind})`;
  if (entry.resume && entry.resume_command != null) {
    console.info(`${RESUME_TRACE} ${where}: RESUME.`);
    return;
  }
  const reason =
    entry.skip_reason ?? (entry.resume ? "resume with no command" : "no skip_reason reported");
  console.info(`${RESUME_TRACE} ${where}: SKIP — ${reason}.`);
}
