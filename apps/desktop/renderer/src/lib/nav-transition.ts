/**
 * NAVIGATION CROSS-FADE — the View Transitions API seam (v2.2.5 follow-up).
 *
 * User feedback: navigating between the main screens (Terminals ↔ Work ↔
 * Knowledge) made the terminal "snap" in ("collé") — the terminal area had no
 * transition, so the persistent DOM node was re-attached instantly with no
 * fade. The decision was to wrap the ROUTE change in Chromium's View
 * Transitions API (`document.startViewTransition`) so the old screen cross-fades
 * into the new one (the API's default ~150-250ms fade), terminal included.
 *
 * ## Why this is safe for the lossless terminal (the critical constraint)
 * The View Transitions API captures a STATIC snapshot of the old DOM, then
 * animates that snapshot out while the new (live) DOM animates in. The callback
 * we pass ONLY runs the route change — it never touches the module-level
 * terminal store: no `destroy()`, no respawn, the same wterm/ghostty instance is
 * detached from the old pane and re-attached (node MOVED, never cloned) into the
 * new one exactly as before. The VT is pure chrome over that swap. The gate
 * (`gate-lossless.browser.test.tsx`) and `nav-transition.test.tsx` both assert
 * the instance identity + spawn count survive a transition.
 *
 * ## Reduced motion + fallback (done_criteria 3 + 4)
 * - `prefers-reduced-motion: reduce` → we run the change instantly, no VT.
 * - `document.startViewTransition` absent (jsdom unit tests, older Chromium) →
 *   we run the change directly; the caller's navigation still happens, no crash.
 *   In the unit suite the API does not exist, so this fallback path is the
 *   DEFAULT one exercised there.
 */

/**
 * The subset of `Document` we call. Declared locally so the module type-checks in
 * the unit (jsdom) environment where `startViewTransition` is absent from the DOM
 * lib types — we feature-detect it at runtime rather than relying on the ambient
 * type being present.
 */
interface ViewTransitionDocument {
  startViewTransition?: (callback: () => void) => unknown;
}

/** Whether the user asked for reduced motion (`prefers-reduced-motion: reduce`). */
function prefersReducedMotion(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return false;
  try {
    return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    // A matchMedia stub that throws (some test envs) must never break navigation.
    return false;
  }
}

/**
 * Run `apply` — the callback that performs the actual navigation / route change —
 * wrapped in a View Transition cross-fade when it is available AND the user has
 * not asked for reduced motion. Otherwise run it directly (the fallback), so
 * navigation always happens and nothing ever crashes.
 *
 * `apply` MUST only mutate the route (call the router's `navigate`). It must NOT
 * spawn/destroy terminals: the persistent terminal instance is meant to SURVIVE
 * the transition untouched (see the module doc).
 *
 * @param apply the navigation mutation to perform (typically `() => navigate(...)`).
 */
export function startNavTransition(apply: () => void): void {
  const doc = document as unknown as ViewTransitionDocument;
  // Fallback path (default in unit tests / old Chromium) OR reduced-motion:
  // apply the navigation instantly, no cross-fade.
  if (typeof doc.startViewTransition !== "function" || prefersReducedMotion()) {
    apply();
    return;
  }
  // Chromium with the API + motion allowed: snapshot old → cross-fade → new.
  doc.startViewTransition(() => {
    apply();
  });
}
