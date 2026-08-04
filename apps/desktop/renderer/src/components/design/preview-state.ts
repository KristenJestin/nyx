/**
 * WHAT THE PREVIEW SURFACE SHOWS — the one decision that used to be an open-ended ladder ending in a
 * spinner (finding `01KYT43KG9BR2J338MS6MVQXZW`, defects 1 and 2).
 *
 * # The defect this closes
 *
 * The proto-viewer chose between an iframe and a spinner with `markup != null ? frame : spinner`.
 * That reads "no markup yet" as "still loading", which is true of exactly ONE of the three ways
 * `markup` can be `null`:
 *
 *   1. a request is IN FLIGHT — the only genuine loading;
 *   2. the request FAILED — the hook holds the readable error, which the ladder never reached because
 *      it tested `markup` first;
 *   3. the hook is DISABLED — no project, no composition: it resolves instantly to
 *      `markup: null, loading: false, error: null` and NOTHING will ever arrive.
 *
 * Case 3 is what the user saw: the tab derived its project id from the Spec's optional dev WORKSPACE
 * (`workspace_context`, `null` for any Spec not anchored on one), so `useDesignRender(null, …)`
 * short-circuited and the surface span for ever. Measured against the live sidecar with the project
 * id the Spec itself carries, the very same composition compiles in 4 ms.
 *
 * # The rule
 *
 * A spinner is shown ONLY while something is actually in flight. Every other outcome — no variant, no
 * project, a render error, a composition that compiled to nothing — is a SENTENCE. This function is
 * total: for any combination of inputs it returns a state, and `loading` requires an in-flight read,
 * so no input can leave the surface pending.
 */

/** What the preview surface must render, exhaustively. */
export type PreviewState =
  /** The page has no variant at all — nothing to preview yet. */
  | { readonly kind: "no-variant" }
  /** Something IS in flight (the catalogue read, or the render itself). The only spinner. */
  | { readonly kind: "loading" }
  /** The render failed — the sidecar's own readable message. */
  | { readonly kind: "error"; readonly message: string }
  /**
   * Nothing can be rendered and nothing is coming: the project could not be resolved, or the variant
   * carries no composition. A DEAD END, said out loud rather than spun on.
   */
  | { readonly kind: "unavailable"; readonly message: string }
  /** The composition compiled to EMPTY markup — it rendered, and it produced nothing. */
  | { readonly kind: "empty" }
  /** The markup to display in the frame. */
  | { readonly kind: "ready"; readonly markup: string };

export interface PreviewInputs {
  /** Does the shown page have a variant to preview at all? */
  readonly hasVariant: boolean;
  /** The project the design catalogue + render are scoped to (`null` = unresolvable). */
  readonly projectId: string | null;
  /** The shown variant's composition (`null`/empty = the variant carries none). */
  readonly compositionJson: string | null;
  /**
   * Is a `design_render` in flight?
   *
   * It used to be joined by a `catalogueLoading` input: the renderer judged the HOLES itself, so it
   * had to wait for the project's catalogue before it could hand the sidecar anything. The sidecar
   * now renders LENIENTLY and names its own holes (finding `01KYT78699XG6Q6P3VZC64W8H8`), so the
   * preview owes the catalogue nothing and the render is the only thing that can be in flight.
   */
  readonly renderLoading: boolean;
  /** The render's readable error, `null` when the last render did not fail. */
  readonly renderError: string | null;
  /** The compiled markup, `null` when there is none (in flight, failed, or never asked). */
  readonly markup: string | null;
}

/**
 * No project id could be resolved — the render and the catalogue are both scoped to one.
 *
 * Worded for EVERY surface that renders a composition, not just the Spec's prototype tab: the same
 * dead end reaches the catalogue tile and the composant detail, where a Spec is nowhere in sight
 * (finding `01KZ3JY9YB4M8789MCWQ2ZT9V8` and its siblings — one decision, one sentence, one place).
 */
export const NO_PROJECT_MESSAGE =
  "No project could be resolved, so the design catalogue and the renderer have nothing to work against.";

/** The variant exists but carries no composition to compile. */
export const NO_COMPOSITION_MESSAGE = "This variant carries no composition to render.";

/** DECIDE what the preview surface shows. Total, pure, and never pending without a reason. */
export function previewState(inputs: PreviewInputs): PreviewState {
  if (!inputs.hasVariant) return { kind: "no-variant" };
  if (inputs.projectId === null) return { kind: "unavailable", message: NO_PROJECT_MESSAGE };
  if (inputs.compositionJson === null || inputs.compositionJson.length === 0) {
    return { kind: "unavailable", message: NO_COMPOSITION_MESSAGE };
  }
  // The error is read BEFORE the markup: a failed render leaves `markup` null, and testing the markup
  // first is exactly what buried the sidecar's message under a spinner.
  if (inputs.renderError !== null) return { kind: "error", message: inputs.renderError };
  if (inputs.renderLoading) return { kind: "loading" };
  if (inputs.markup === null) return { kind: "loading" };
  if (inputs.markup.trim().length === 0) return { kind: "empty" };
  return { kind: "ready", markup: inputs.markup };
}
