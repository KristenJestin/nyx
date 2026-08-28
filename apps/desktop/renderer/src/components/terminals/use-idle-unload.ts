import { useEffect, useRef } from "react";

import { nyxBridge } from "@/bridge";
import { recordIdleUnloadSweep, useIdleUnloadSeam } from "./idle-unload-seam";

/** Default cadence of the idle-unload sweep — once a minute is plenty (the threshold is minutes). */
const DEFAULT_SWEEP_INTERVAL_MS = 60_000;
/** Default inactivity threshold before an idle, unvisited agent session is unloaded — 15 minutes
 *  (mirrors the sidecar's `DEFAULT_UNLOAD_THRESHOLD_SECS`, the value the spike recommends). */
const DEFAULT_THRESHOLD_SECS = 15 * 60;

/**
 * `use-idle-unload` — the RENDERER driver of the v2.5.5 IDLE-SESSION UNLOAD (task
 * `01KX8Z6T1J2AC65DHHZTCQ26Z4`). On a timer it asks the sidecar to reclaim the resources of
 * DORMANT agent sessions: `unload_idle_sessions` closes the PTY of every agent-hosting terminal
 * that is Idle (NEVER Working/Waiting — the sidecar enforces "une session ACTIVE n'est JAMAIS
 * unloadée"), NOT currently visited, and idle (no PTY I/O) beyond the threshold.
 *
 * The renderer only supplies POLICY INPUTS — which terminals are currently VISITED (focused) and
 * the inactivity threshold; the authoritative safety decision (activity gate + the "action en
 * cours" exception) lives in the sidecar (`nyx_core::agent_unload::decide_unload`), so a race on
 * the visited set can at worst KEEP a session loaded, never unload an active one.
 *
 * ## Resume-au-retour, IN-RUN (no new mechanism)
 * An unloaded session stays `active` in the DB, and the sidecar itself RE-PARKS its resume at unload:
 * it rebuilds `claude --resume <id>` via the SAME `decide_resume` policy and re-arms the record's
 * one-shot guard (`ResumeParks::rearm`). So when the user revisits the terminal WITHIN THE SAME RUN,
 * the PTY re-spawns and the phase-2 lazy park+inject re-injects the resume automatically — it does
 * NOT wait for `use-lazy-resume` to re-push the plan (that hook pushes each workspace's plan at most
 * ONCE per app run, so it would never re-park after an in-run unload). This hook adds NO resume code;
 * the in-run resume is entirely the sidecar's re-park (review `01KX9CP96KVK6BTHY3ZSWY1E67`, finding 1).
 *
 * ## Only resume-eligible sessions are unloaded (finding 2)
 * The sidecar unloads ONLY a session it could resume on revisit (the `resume_agent_sessions` toggle
 * is the master switch): a toggle-OFF (default) or otherwise non-resumable idle session is KEPT
 * loaded rather than killed without recourse.
 *
 * ## Best-effort
 * The sweep is fire-and-forget: a failed invoke (sidecar busy / not ready) just means no unload
 * this tick — a later tick retries. It never surfaces an error to the page. The latest `visited`
 * set is read from a ref so a focus change never re-arms the timer.
 *
 * ## VISITED IS EVERY VISIBLE PANE
 * `visitedTerminalIds` is a POLICY INPUT and its meaning is "on screen", not "selected". With the
 * tiling, several terminals are visible at once (`usePaneLayout().visibleTerminalIds` — every leaf
 * of the pane tree), and handing only the focused one would let the sidecar unload the sessions of
 * panes the user is LOOKING AT. That is a correctness condition of the tiling, not an
 * optimisation, and it is why the caller must pass the visible set.
 *
 * Because the invoke is fire-and-forget, that claim is otherwise unobservable — so every sweep is
 * recorded on the read seam (`idle-unload-seam.ts`, `window.__nyxIdleUnload`), which is inert in
 * production and is the only vantage point a piloted session has on these arguments.
 */
export function useIdleUnload(
  /** The terminals to declare VISITED — every VISIBLE pane, not the focused one (see above). */
  visitedTerminalIds: readonly string[],
  options?: { readonly intervalMs?: number; readonly thresholdSecs?: number },
): void {
  const intervalMs = options?.intervalMs ?? DEFAULT_SWEEP_INTERVAL_MS;
  const thresholdSecs = options?.thresholdSecs ?? DEFAULT_THRESHOLD_SECS;

  // Publish the read seam for as long as this hook is mounted (refcounted — several surfaces may
  // sweep at once, and the first unmount must not take the global away from the others).
  useIdleUnloadSeam();

  // Keep the CURRENT visited set in a ref so the interval callback always reads the live focus
  // without re-arming the timer on every selection change (which would reset the sweep clock).
  const visitedRef = useRef<readonly string[]>(visitedTerminalIds);
  visitedRef.current = visitedTerminalIds;

  useEffect(() => {
    const sweep = () => {
      const visited = visitedRef.current;
      // Recorded at the instant of the invoke, with the very arguments sent — the seam must report
      // what the sidecar received, not what the state looked like when the promise settled.
      recordIdleUnloadSweep({ at: Date.now(), thresholdSecs, visited });
      void nyxBridge.invoke("unload_idle_sessions", { thresholdSecs, visited }).catch(() => {
        // Best-effort: a failed sweep means no unload this tick; a later tick retries.
      });
    };
    const timer = setInterval(sweep, intervalMs);
    return () => clearInterval(timer);
  }, [intervalMs, thresholdSecs]);
}
