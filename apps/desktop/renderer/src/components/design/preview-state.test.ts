import { describe, expect, it } from "vitest";

import {
  NO_COMPOSITION_MESSAGE,
  NO_PROJECT_MESSAGE,
  previewState,
  type PreviewInputs,
} from "./preview-state";

/**
 * DONE-CRITERION (finding `01KYT43KG9BR2J338MS6MVQXZW`) — NO LOADING STATE CAN STAY PENDING.
 *
 * The defect was structural: the surface chose `markup != null ? frame : spinner`, which reads "no
 * markup" as "still loading" — true only while a request is in flight, false for a failed render and
 * false for a DISABLED hook (which is what a `null` project id produces, and what the user actually
 * hit). These tests pin the rule the other way round: a spinner requires something in flight.
 */

function inputs(over: Partial<PreviewInputs> = {}): PreviewInputs {
  return {
    hasVariant: true,
    projectId: "p1",
    compositionJson: '{"key":"Root"}',
    renderLoading: false,
    renderError: null,
    markup: "<div>x</div>",
    ...over,
  };
}

describe("previewState", () => {
  it("shows the markup when a render came back", () => {
    expect(previewState(inputs())).toEqual({ kind: "ready", markup: "<div>x</div>" });
  });

  it("says so when the page has no variant at all", () => {
    expect(previewState(inputs({ hasVariant: false })).kind).toBe("no-variant");
  });

  /**
   * THE BUG, pinned. A `null` project id disables the render hook, which then resolves INSTANTLY to
   * `markup: null, loading: false, error: null`. Nothing is coming, ever. It must read as a dead end,
   * never as a spinner.
   */
  it("names the dead end when no project could be resolved — never a spinner", () => {
    const state = previewState(inputs({ projectId: null, markup: null }));
    expect(state).toEqual({ kind: "unavailable", message: NO_PROJECT_MESSAGE });
  });

  it("names the dead end when the variant carries no composition — never a spinner", () => {
    expect(previewState(inputs({ compositionJson: null, markup: null }))).toEqual({
      kind: "unavailable",
      message: NO_COMPOSITION_MESSAGE,
    });
    expect(previewState(inputs({ compositionJson: "", markup: null })).kind).toBe("unavailable");
  });

  /** The error used to be unreachable: the ladder tested `markup` first, and a failure leaves it null. */
  it("surfaces the render error rather than the absent markup it leaves behind", () => {
    expect(previewState(inputs({ renderError: "unknown component 'x'", markup: null }))).toEqual({
      kind: "error",
      message: "unknown component 'x'",
    });
  });

  it("prefers the error even when a stale markup is still in hand", () => {
    expect(previewState(inputs({ renderError: "boom" })).kind).toBe("error");
  });

  it("spins ONLY while something is in flight", () => {
    expect(previewState(inputs({ renderLoading: true, markup: null })).kind).toBe("loading");
    // The one frame between the composition landing and the render effect running.
    expect(previewState(inputs({ markup: null })).kind).toBe("loading");
  });

  it("says a composition compiled to nothing instead of showing an empty frame", () => {
    expect(previewState(inputs({ markup: "" })).kind).toBe("empty");
    expect(previewState(inputs({ markup: "   \n " })).kind).toBe("empty");
  });

  /**
   * TOTALITY: over the whole cross-product of the inputs, `loading` is returned ONLY when something is
   * genuinely in flight. Anything else that would have spun is now a sentence.
   */
  it("never reports loading without an in-flight read", () => {
    const bools = [false, true];
    for (const hasVariant of bools) {
      for (const projectId of [null, "p1"]) {
        for (const compositionJson of [null, "", '{"key":"Root"}']) {
          for (const renderLoading of bools) {
            for (const renderError of [null, "boom"]) {
              for (const markup of [null, "", "<b/>"]) {
                const state = previewState({
                  hasVariant,
                  projectId,
                  compositionJson,
                  renderLoading,
                  renderError,
                  markup,
                });
                if (state.kind !== "loading") continue;
                const inFlight =
                  renderLoading ||
                  // the transient frame before the render effect fires
                  (markup === null && renderError === null);
                expect(inFlight).toBe(true);
                // …and a spinner is only ever shown against a real, project-scoped composition.
                expect(hasVariant).toBe(true);
                expect(projectId).not.toBeNull();
                expect(compositionJson).toBeTruthy();
              }
            }
          }
        }
      }
    }
  });
});
