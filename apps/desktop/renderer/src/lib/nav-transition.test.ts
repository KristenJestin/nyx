import { afterEach, describe, expect, it, vi } from "vitest";

import { startNavTransition } from "./nav-transition";

/**
 * Unit coverage for the navigation cross-fade seam. jsdom has NO
 * `document.startViewTransition` and NO real `matchMedia`, so we install/remove
 * doubles per case to drive each of the three branches:
 *   - API present + motion allowed → the transition wraps the navigation;
 *   - API absent (the jsdom default) → fallback runs the navigation directly;
 *   - `prefers-reduced-motion: reduce` → no transition, navigation runs directly.
 */

interface VTDoc {
  startViewTransition?: (cb: () => void) => unknown;
}
const vtDoc = document as unknown as VTDoc;

/** Install a spy `startViewTransition` that (by default) runs its callback synchronously. */
function installViewTransition(runCallback = true): ReturnType<typeof vi.fn> {
  const spy = vi.fn((cb: () => void) => {
    if (runCallback) cb();
    return {
      finished: Promise.resolve(),
      ready: Promise.resolve(),
      updateCallbackDone: Promise.resolve(),
    };
  });
  vtDoc.startViewTransition = spy as unknown as VTDoc["startViewTransition"];
  return spy;
}

/** Force `prefers-reduced-motion: reduce` (or not) via a `matchMedia` stub. */
function installMatchMedia(reduced: boolean): void {
  vi.stubGlobal(
    "matchMedia",
    vi.fn((query: string) => ({
      matches: reduced && query.includes("prefers-reduced-motion"),
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      onchange: null,
      dispatchEvent: () => false,
    })),
  );
}

afterEach(() => {
  delete vtDoc.startViewTransition;
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("startNavTransition", () => {
  it("wraps the navigation in document.startViewTransition when the API exists and motion is allowed", () => {
    installMatchMedia(false);
    const vt = installViewTransition();
    const apply = vi.fn();

    startNavTransition(apply);

    // The View Transition was started, and the navigation ran inside its callback.
    expect(vt).toHaveBeenCalledTimes(1);
    expect(apply).toHaveBeenCalledTimes(1);
  });

  it("FALLBACK: runs the navigation directly (no crash) when startViewTransition is absent", () => {
    installMatchMedia(false);
    // No startViewTransition installed — the jsdom default, and the app's test path.
    expect(vtDoc.startViewTransition).toBeUndefined();
    const apply = vi.fn();

    // Must not throw, and must still navigate.
    expect(() => startNavTransition(apply)).not.toThrow();
    expect(apply).toHaveBeenCalledTimes(1);
  });

  it("REDUCED MOTION: skips the transition and navigates instantly when prefers-reduced-motion is reduce", () => {
    installMatchMedia(true);
    const vt = installViewTransition();
    const apply = vi.fn();

    startNavTransition(apply);

    // The transition was NOT started (instant nav), but the navigation still ran.
    expect(vt).not.toHaveBeenCalled();
    expect(apply).toHaveBeenCalledTimes(1);
  });

  it("does not throw when matchMedia itself throws (defensive)", () => {
    vi.stubGlobal(
      "matchMedia",
      vi.fn(() => {
        throw new Error("no matchMedia");
      }),
    );
    const vt = installViewTransition();
    const apply = vi.fn();

    expect(() => startNavTransition(apply)).not.toThrow();
    // A throwing matchMedia is treated as "motion allowed" → the VT still wraps nav.
    expect(vt).toHaveBeenCalledTimes(1);
    expect(apply).toHaveBeenCalledTimes(1);
  });
});
