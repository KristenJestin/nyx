import { describe, expect, it } from "vitest";

import {
  DOT_PRESENCE,
  dotAppearTransition,
  dotCrossfadeTransition,
  dotExitTransition,
} from "./dot-motion";

/**
 * The run-state dots delegate their reduced-motion decision to these PURE
 * resolvers (mirroring `item-motion`/`section-motion`). Testing them directly is
 * the deterministic seam for the `prefers-reduced-motion` behaviour: Motion's
 * `useReducedMotion()` reads a process-wide cached media-query that jsdom cannot
 * flip per-test, so we assert the branch HERE rather than through a live render.
 */
describe("dot-motion (shared run-state dot transitions)", () => {
  it("APPEAR: a spring pop normally, an INSTANT snap under reduced motion", () => {
    const normal = dotAppearTransition(false);
    expect(normal).toMatchObject({ type: "spring" });
    // Reduced ⇒ zero-duration (instant), no spring/scale.
    expect(dotAppearTransition(true)).toEqual({ duration: 0 });
  });

  it("DISAPPEAR: a short fade normally, an INSTANT leave under reduced motion", () => {
    const normal = dotExitTransition(false);
    expect(normal).toMatchObject({ duration: expect.any(Number) });
    expect((normal as { duration: number }).duration).toBeGreaterThan(0);
    expect(dotExitTransition(true)).toEqual({ duration: 0 });
  });

  it("COLOUR cross-fade: a short tween normally, an INSTANT colour snap under reduced motion", () => {
    const normal = dotCrossfadeTransition(false);
    expect((normal as { duration: number }).duration).toBeGreaterThan(0);
    // Reduced ⇒ the colour swap snaps instantly (no cross-fade overlap).
    expect(dotCrossfadeTransition(true)).toEqual({ duration: 0 });
  });

  it("exposes a stable appear/exit presence target (scale-from/to 0 + fade)", () => {
    expect(DOT_PRESENCE.initial).toEqual({ scale: 0, opacity: 0 });
    expect(DOT_PRESENCE.animate).toEqual({ scale: 1, opacity: 1 });
    expect(DOT_PRESENCE.exit).toEqual({ scale: 0, opacity: 0 });
  });
});
