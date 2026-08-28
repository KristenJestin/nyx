import { useEffect } from "react";

/**
 * THE IDLE-UNLOAD READ SEAM — `window.__nyxIdleUnload()`, published from ONE place.
 *
 *   - `__nyxIdleUnload()` → the LAST sweep this renderer asked the sidecar for, or `null` when
 *     none has run yet: `{ at, thresholdSecs, visited }`.
 *
 * WHY IT EXISTS. `useIdleUnload` fires `unload_idle_sessions` as a `void … .catch(() => {})` —
 * deliberately best-effort, and therefore completely OPAQUE: nothing it sends can be observed
 * from a real-drive test. That matters because this PRD's central rule — the set of terminals
 * PROTECTED from the unload sweep is every VISIBLE pane, not just the focused one — is a claim
 * about the arguments of that very call. Without a seam the rule cannot be checked in a piloted
 * session at all; with one it is a single read.
 *
 * WHY A SEAM AND NOT AN INSTRUMENTED BRIDGE. Every other route is closed BY DESIGN, and those
 * closures are good properties this module does NOT touch:
 *   - `window.nyxCore` is deep-frozen and non-configurable, so spying on it fails SILENTLY;
 *   - the preload is sandboxed and `require` is unavailable even in its isolated world, so
 *     `ipcRenderer` cannot be patched;
 *   - back-dating the database proves nothing: `idle_for` comes from `pty.agent_idle_durations()`,
 *     a runtime value;
 *   - waiting for the real effect needs resume-eligible sessions plus 15 minutes of inactivity,
 *     and discriminates nothing when none is eligible.
 * So the observation point is HERE, on the renderer side, exactly like the per-terminal buffer
 * seam. The bridge, `nyxCore`, the preload, the sandbox and `contextIsolation` are untouched.
 *
 * IT OBSERVES, IT DOES NOT DECIDE. Recording a sweep changes no cadence, no threshold and no
 * unload decision — those stay where they are (the hook's timer and the sidecar's
 * `decide_unload`). Reading the seam has no effect at all.
 *
 * INERT IN PRODUCTION: nothing in the application reads `__nyxIdleUnload`. It is a CONTRACT, and
 * this is its exact reader inventory — every file naming it, i.e. `grep -rln "__nyxIdleUnload" e2e/`
 * (keep the two in sync; a refactor of this module breaks precisely these):
 *   - `e2e/README.md` — documents the seam;
 *   - (no e2e spec drives it yet — the renderer-side proof lives in `idle-unload-seam.test.tsx`
 *     and `idle-unload-seam.browser.test.tsx`).
 *
 * WHY A REFCOUNT. Same rule as `deck-seam`: the seam may have MORE THAN ONE host (any surface
 * mounting `useIdleUnload`), and with a copy of the publication in each, the first unmount would
 * delete the global the other is still publishing. So the global is owned HERE: installed on the
 * first claim, removed only when the last holder releases it. The published function is a
 * stateless reader over the module's own record, so which host claimed first is irrelevant.
 */

/** One recorded sweep — the arguments `unload_idle_sessions` was called with, plus when. */
export interface IdleUnloadSweep {
  /** `Date.now()` at the instant the sweep was issued (it ADVANCES on every tick). */
  readonly at: number;
  /** The inactivity threshold handed to the sidecar, in seconds. */
  readonly thresholdSecs: number;
  /** The terminal ids declared VISITED — the tiling's VISIBLE set, not the selection. */
  readonly visited: readonly string[];
}

/** The window field this seam owns. */
interface IdleUnloadSeamWindow {
  __nyxIdleUnload?: () => IdleUnloadSweep | null;
}

/** How many live holders claim the seam right now (0 ⇒ the global is absent). */
let holders = 0;
/** The last sweep, or `null` before the first one. */
let lastSweep: IdleUnloadSweep | null = null;

/**
 * Record the sweep being issued. Called by {@link import("./use-idle-unload").useIdleUnload} at the
 * instant of the invoke, UNCONDITIONALLY — a sweep that fired before anyone claimed the seam must
 * still be the one a later reader sees, so the record does not depend on the claim order. The
 * `visited` list is COPIED: the caller hands over a live ref whose contents change with the focus.
 */
export function recordIdleUnloadSweep(sweep: IdleUnloadSweep): void {
  lastSweep = { at: sweep.at, thresholdSecs: sweep.thresholdSecs, visited: [...sweep.visited] };
}

/**
 * Claim the seam, publishing it if nobody had. Returns the RELEASE function: the global is deleted
 * only when the last holder releases (calling it twice is a no-op).
 */
export function claimIdleUnloadSeam(): () => void {
  if (typeof window === "undefined") return () => {};
  const win = window as unknown as IdleUnloadSeamWindow;
  holders += 1;
  if (holders === 1) win.__nyxIdleUnload = () => lastSweep;
  let released = false;
  return () => {
    if (released) return;
    released = true;
    holders = Math.max(0, holders - 1);
    if (holders === 0) delete win.__nyxIdleUnload;
  };
}

/** Publish the seam for as long as the calling component is mounted. */
export function useIdleUnloadSeam(): void {
  useEffect(() => claimIdleUnloadSeam(), []);
}

/** Live holder count — a test seam for the refcount itself. */
export function __idleUnloadSeamHolders(): number {
  return holders;
}

/** Forget the recorded sweep — a test seam, so one spec cannot read another's leftovers. */
export function __resetIdleUnloadSeam(): void {
  lastSweep = null;
}
