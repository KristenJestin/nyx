import { useEffect, useState } from "react";

import { errorText, nyxBridge } from "@/bridge";

/**
 * `useDesignRender` — compile a COMPOSITION to MARKUP in the sidecar (`design_render`, task #6) for
 * display in a [`CompositionFrame`] (v2.7 Phase 3). The Electron renderer compiles nothing; it only
 * asks the sidecar for the markup. `null` inputs disable the hook.
 *
 * # The render is LENIENT, and the holes come WITH the markup
 *
 * `design_render` used to be all-or-nothing: one node naming a component the catalogue does not
 * carry and the whole page came back as an error line. A composition under design ALWAYS holds
 * `[NEW]` nodes — the agent proposes a component before anyone builds it — so "mostly renderable,
 * with holes" is the NORMAL state of a page in flight, and a preview that refuses to draw until the
 * last hole is filled is a preview nobody can use.
 *
 * The sidecar now answers `{ markup, node_count, unrendered, unrendered_count }` (finding
 * `01KYT78699XG6Q6P3VZC64W8H8`): every unrenderable node becomes a NAMED placeholder at the exact
 * spot it occupies, and one [`UnrenderedNode`] entry says which node, which component, why, and what
 * to do about it. The renderer computes NO notion of a hole of its own — it used to, in a
 * `composition-holes.ts` that knew only two of the five reasons, so a template carrying a partial or
 * a block helper was a hole the frame could not name. That twin was DELETED with this: this hook
 * carries the wire shape, and a host DISPLAYS the list.
 *
 * A composition that does not PARSE is the only refusal left: it surfaces as `error`, with the
 * sidecar's readable message, never a silent empty frame.
 *
 * # The SCOPED CSS SLOTS come with the markup too
 *
 * The answer also carries `{ css_slots, css_slot_count }` (finding `01KYVMTWB71R0D0CT1SV88J8DX`,
 * displayed by `01KYVVEQZW2Y2VY7ZAPK5RECZS`): every strict CSS exception the page reaches, ONE entry
 * per component, with its scope, its compiled CSS, the constructs it uses and the reason it exists —
 * and, for a stored slot that no longer compiles, the refusal that kept it out. The renderer
 * COMPILES nothing and SCOPES nothing; it displays what the sidecar already decided. The MARK on the
 * node is not in this list at all: it is `data-nyx-css` IN THE MARKUP, stamped by the sidecar on the
 * component's root element — the frame styles that attribute, it never re-derives which node is
 * concerned.
 *
 * # THE PARCOURS NEEDS THE PROTOTYPE (finding `01KZ3JN69E32FYBF01NBE4FWJD`)
 *
 * A node may state the page it OPENS (`navigate_to`, a `prototype_pages.id`), and the render resolves
 * that id against the prototype's page DIRECTORY to stamp the slug the shim navigates with. The
 * directory is what `prototypeId` supplies — and nothing supplies it unless a caller passes it, which
 * no caller did. `design_render` then resolved every link against an EMPTY directory, so every one of
 * them came back `unknown_page`: marked `data-nyx-page-missing`, carrying no `data-nyx-page`,
 * un-navigable. Not a degraded parcours — NO parcours, in an app where every unit test passed, because
 * the argument is optional on the wire and its absence is indistinguishable from a page-less
 * composition.
 *
 * So the id travels with the render, and what came back of it is CARRIED here rather than dropped:
 * {@link NavigationLink}, one entry per linking node whatever became of it. A host that shows pages
 * DISPLAYS that list — a link that works and a link whose page is gone are both facts about the page
 * under review, and for the second one this is the only warning there will ever be.
 */

/**
 * WHAT BECAME OF ONE NODE'S LINK — the three stable machine codes the sidecar answers with
 * (`nyx_core::render::NavigationStatus::code`). There is no code meaning "nothing happened": a link
 * that could not be honoured is NAMED, which is the difference between a dead click and a diagnosis.
 *
 * PINNED IN BOTH DIRECTIONS by `contract/design-fixtures.json` (`navigation.statuses`), exactly as
 * {@link UnrenderedReason} is.
 */
export type NavigationStatus = "linked" | "unknown_page" | "no_anchor";

/**
 * The SHORT phrase each status reads as — `navigation.statuses[].short` of the fixture, verbatim.
 */
export const NAVIGATION_STATUS_LABELS: Record<NavigationStatus, string> = {
  linked: "navigates",
  unknown_page: "page is gone",
  no_anchor: "nothing to click",
};

/** The label of a status CODE — an unknown code is shown VERBATIM rather than swallowed. */
export function navigationStatusLabel(status: string): string {
  return NAVIGATION_STATUS_LABELS[status as NavigationStatus] ?? status;
}

/**
 * ONE inter-page link the render met, resolved — the wire shape, snake_case as it arrives.
 *
 * The model stores an ID (`page_id`), because a slug is renamed; the readable half (`page`, the slug
 * the shim posts, and `page_title`, what a human reads) is resolved AT THE RENDER, so a rename costs
 * nothing and neither half can go stale. Both are `null` exactly when the id resolved to nothing.
 */
export interface NavigationLink {
  /** The LINKING node's `node_key` PATH — the same anchor the comments and the seam use. */
  readonly node_key: string;
  /** The destination as STORED: a `prototype_pages.id`. */
  readonly page_id: string;
  /** The destination's SLUG, resolved at this render (`data-nyx-page`); `null` when unresolved. */
  readonly page: string | null;
  /** The destination's human TITLE, resolved at this render; `null` when unresolved. */
  readonly page_title: string | null;
  /** {@link NavigationStatus} — `linked` is the only one that navigates. */
  readonly status: string;
  /** The full sentence: what happened and what to do about it. Empty for a link that resolved. */
  readonly detail: string;
}

/**
 * WHY one node of a composition could not be rendered — the five stable machine codes the sidecar
 * answers with (`nyx_core::render::UnrenderedReason::code`).
 *
 * PINNED IN BOTH DIRECTIONS by `contract/design-fixtures.json` (`unrendered_nodes.reasons`): a
 * variant added in Rust with no entry here fails the arrimage test, and an entry here with no
 * fixture line fails it too. That file is the ONE place this vocabulary lives.
 */
export type UnrenderedReason =
  | "not_in_catalogue"
  | "no_template"
  | "template_has_logic"
  | "template_has_partial"
  | "render_failed";

/**
 * The SHORT phrase each reason reads as — `reasons[].short` of the fixture, verbatim. It is the same
 * wording the sidecar puts inside the placeholder markup (`[UNRENDERED Button — no template yet]`),
 * so the badge under the frame and the box in the frame say the same thing about the same node.
 */
export const UNRENDERED_REASON_LABELS: Record<UnrenderedReason, string> = {
  not_in_catalogue: "not in the catalogue",
  no_template: "no template yet",
  template_has_logic: "template carries logic",
  template_has_partial: "template includes a partial",
  render_failed: "render failed",
};

/**
 * The label of a reason CODE. An unknown code is shown VERBATIM rather than swallowed: the fixture
 * makes that unreachable at test time, and if it ever happened the raw code still names the defect,
 * where a blank badge would hide it.
 */
export function unrenderedReasonLabel(reason: string): string {
  return UNRENDERED_REASON_LABELS[reason as UnrenderedReason] ?? reason;
}

/**
 * The four constructs a SCOPED CSS SLOT may exist for — the ONLY things Tailwind utilities cannot
 * express (`nyx_core::css_slot::SlotFeature::code`). A slot that uses none of them is refused
 * (`no_escape_hatch`): it would be CSS written INSTEAD of the design system.
 *
 * PINNED IN BOTH DIRECTIONS by `contract/design-fixtures.json` (`css_slot.features`), exactly as
 * {@link UnrenderedReason} is: a feature added in Rust with no label here fails the arrimage test,
 * and a label invented here with no fixture line fails it too.
 */
export type CssSlotFeature = "keyframes" | "pseudo_element" | "has" | "container";

/** How each feature READS in the review card — the construct itself, in CSS's own words. */
export const CSS_SLOT_FEATURE_LABELS: Record<CssSlotFeature, string> = {
  keyframes: "@keyframes",
  pseudo_element: "::before / ::after",
  has: ":has(…)",
  container: "@container",
};

/**
 * WHY a stored slot did not compile — the nineteen stable codes
 * (`nyx_core::css_slot::Refusal::code`), pinned in both directions by
 * `contract/design-fixtures.json` (`css_slot.refusals`).
 *
 * They reach this surface through `css_slots[].refused`: the write path refuses such a slot in the
 * first place, so a refused slot HERE is one that was stored when the rules were laxer, or whose
 * token layer moved under it. It is NAMED rather than dropped — the same principle as an
 * unrenderable node.
 */
export type CssSlotRefusal =
  | "blank"
  | "no_reason"
  | "reason_too_long"
  | "too_large"
  | "markup"
  | "important"
  | "external_resource"
  | "at_rule"
  | "nested_rule"
  | "unterminated"
  | "bad_declaration"
  | "global_selector"
  | "escaping_selector"
  | "misplaced_scope"
  | "empty_selector"
  | "hardcoded_colour"
  | "hardcoded_length"
  | "unknown_token"
  | "no_escape_hatch";

/** The SHORT phrase each refusal reads as. The sidecar's own `detail` says the rest, verbatim. */
export const CSS_SLOT_REFUSAL_LABELS: Record<CssSlotRefusal, string> = {
  blank: "empty CSS",
  no_reason: "no reason given",
  reason_too_long: "reason too long",
  too_large: "past the size cap",
  markup: "markup inside the CSS",
  important: "!important",
  external_resource: "external resource",
  at_rule: "at-rule not admitted",
  nested_rule: "nested rule",
  unterminated: "unterminated block",
  bad_declaration: "not a declaration",
  global_selector: "global selector",
  escaping_selector: "selector escaping the scope",
  misplaced_scope: "misplaced :scope",
  empty_selector: "empty selector",
  hardcoded_colour: "hard-coded colour",
  hardcoded_length: "hard-coded length",
  unknown_token: "unknown token",
  no_escape_hatch: "no escape hatch used",
};

/** The label of a feature CODE — an unknown code is shown VERBATIM rather than swallowed. */
export function cssSlotFeatureLabel(feature: string): string {
  return CSS_SLOT_FEATURE_LABELS[feature as CssSlotFeature] ?? feature;
}

/** The label of a refusal CODE — an unknown code is shown VERBATIM rather than swallowed. */
export function cssSlotRefusalLabel(refusal: string): string {
  return CSS_SLOT_REFUSAL_LABELS[refusal as CssSlotRefusal] ?? refusal;
}

/**
 * ONE scoped CSS slot the render MET — the wire shape, snake_case as it arrives.
 *
 * IT BELONGS TO THE COMPONENT, NOT TO A NODE. A `Spinner` placed thirty times carries ONE slot; the
 * nodes only POINT at it (`ElectedNode.css_scope`). Every display of it must say so, or it invites
 * the reader to believe they are looking at an exception of that one placement.
 */
export interface RenderedCssSlot {
  /** The scope id stamped on the component's root element(s) — `data-nyx-css` IN THE MARKUP. */
  readonly scope_id: string;
  /** The catalogue component the slot belongs to. */
  readonly component: string;
  /** The selector the CSS is bounded to, literally (`[data-nyx-css~="c-spinner-…"]`). */
  readonly scope: string;
  /** WHY the exception exists, in the agent's words. Empty only for a pre-mandatory row. */
  readonly reason: string;
  /** Which of the four infeasible-in-utilities constructs it exercises ({@link CssSlotFeature}). */
  readonly features: readonly string[];
  /** The size of the AUTHORED CSS. */
  readonly bytes: number;
  /** The COMPILED, scoped CSS — what the frame really applied. EMPTY when the slot was refused. */
  readonly css: string;
  /** The refusal CODE when the stored slot no longer compiles ({@link CssSlotRefusal}); else `null`. */
  readonly refused: string | null;
  /** The refusal's full wording — the sidecar's own sentence, shown verbatim; `null` when emitted. */
  readonly detail: string | null;
}

/** ONE node the sidecar could not render, NAMED — the wire shape, snake_case as it arrives. */
export interface UnrenderedNode {
  /** The `/`-joined node path (`Root/Filters/Reset`) — the anchor every other surface names it by. */
  readonly node_key: string;
  /** The catalogue component the node asked for (`null` for an `[inline]` node). */
  readonly component: string | null;
  readonly reason: UnrenderedReason;
  /** The full explanation, correction included — the sidecar's own wording, shown verbatim. */
  readonly detail: string;
}

export interface UseDesignRender {
  markup: string | null;
  loading: boolean;
  /** The readable sidecar error message when the last render failed, else `null`. */
  error: string | null;
  /** How many nodes the walk visited, placeholders included. `0` when nothing was rendered. */
  nodeCount: number;
  /**
   * The nodes that came back as placeholders — EMPTY when everything rendered.
   *
   * **ORDER: depth-first, CHILDREN BEFORE THEIR PARENT.** A hole nested under another hole is listed
   * FIRST. That is the order the sidecar's walk records and the fixture pins, and a display that
   * reads it as a top-down pre-order would state the hierarchy upside down.
   */
  unrendered: readonly UnrenderedNode[];
  /**
   * The SCOPED CSS SLOTS this render met — the strict escape hatches the page really carries, one
   * entry per COMPONENT (never per placement), EMPTY when it carries none.
   *
   * A slot that no longer compiles is IN this list with `refused` filled and `css` empty: it is
   * named, not dropped — same principle as {@link UnrenderedNode}.
   */
  cssSlots: readonly RenderedCssSlot[];
  /** `css_slot_count` — the number the chrome's chip reports, as the sidecar counted it. */
  cssSlotCount: number;
  /**
   * The INTER-PAGE LINKS this render met — every linking node, honoured or not, EMPTY when the page
   * links nowhere. It is empty for another reason too, and the two are worth telling apart: a render
   * given no `prototypeId` has no page directory, so its links come back `unknown_page` — present in
   * this list, and named.
   */
  navigation: readonly NavigationLink[];
  /** `navigation_count` — how many linking nodes the walk met, as the sidecar counted them. */
  navigationCount: number;
  /** `navigation_broken_count` — how many of them do NOT navigate (anything but `linked`). */
  navigationBrokenCount: number;
}

/** What the LAST answer said — the fields that travel together, replaced in one `set`. */
type RenderOutcome = Pick<
  UseDesignRender,
  | "markup"
  | "nodeCount"
  | "unrendered"
  | "cssSlots"
  | "cssSlotCount"
  | "navigation"
  | "navigationCount"
  | "navigationBrokenCount"
>;

/** The empty answer — the shape a disabled or failed render resolves to. */
const NO_RENDER: RenderOutcome = {
  markup: null,
  nodeCount: 0,
  unrendered: [],
  cssSlots: [],
  cssSlotCount: 0,
  navigation: [],
  navigationCount: 0,
  navigationBrokenCount: 0,
};

/** The `design_render` payload, read defensively: a shell that answers less must not throw here. */
interface RenderResponse {
  markup?: string;
  node_count?: number;
  unrendered?: readonly UnrenderedNode[];
  css_slots?: readonly RenderedCssSlot[];
  css_slot_count?: number;
  navigation?: readonly NavigationLink[];
  navigation_count?: number;
  navigation_broken_count?: number;
}

export function useDesignRender(
  projectId: string | null,
  compositionJson: string | null,
  /**
   * THE PROTOTYPE THE COMPOSITION BELONGS TO — the page directory the `navigate_to` of each node is
   * resolved against. Optional on the wire (a composition that links nowhere owes it nothing), but a
   * host that renders a PROTOTYPE PAGE must pass it: without it the directory is empty and every link
   * of the page comes back `unknown_page`, un-navigable and indistinguishable from a page that was
   * really deleted (finding `01KZ3JN69E32FYBF01NBE4FWJD`).
   */
  prototypeId: string | null = null,
  /**
   * THE AXES THE SHIM WILL PILOT — the very `states_json` this host hands the frame as
   * [`CompositionFrame.axes`], i.e. `window.__nyxAxes` (finding `01KZ41YK39SGNKF6XV87A35GPN`).
   *
   * It is not there to be RESOLVED: the shim applies it live over the whole document, which is what
   * the chip-bar and the in-place flip are. It is there so each axis has exactly ONE applier. Every
   * axis a NODE of the composition declares and the shim does not is baked into that node's markup
   * by the sidecar — the only way a node's own `variant` / `size` / `loading` can act, since nothing
   * in the frame can move them. A host that hands the frame axes and NOT this would have the shim
   * and the render fighting over the same names; one that hands neither gets every node's own axes
   * baked, which is right for a thumbnail.
   */
  subjectStatesJson: string | null = null,
): UseDesignRender {
  const [result, setResult] = useState<RenderOutcome>(NO_RENDER);
  const [loading, setLoading] = useState<boolean>(projectId !== null && compositionJson !== null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    if (projectId === null || compositionJson === null) {
      setResult(NO_RENDER);
      setLoading(false);
      setError(null);
      return () => {
        cancelled = true;
      };
    }
    setLoading(true);
    void nyxBridge
      .invoke<RenderResponse>("design_render", {
        projectId,
        compositionJson,
        // OMITTED rather than sent as `null` when there is none: the argument is optional on the
        // sidecar side and `arg_opt_str` reads an absent key, not a null one.
        ...(prototypeId !== null ? { prototypeId } : {}),
        ...(subjectStatesJson !== null ? { subjectStatesJson } : {}),
      })
      .then((res) => {
        if (cancelled) return;
        const cssSlots = res.css_slots ?? [];
        const navigation = res.navigation ?? [];
        setResult({
          markup: res.markup ?? null,
          nodeCount: res.node_count ?? 0,
          unrendered: res.unrendered ?? [],
          cssSlots,
          // The sidecar's OWN count when it sent one (that is the chip's number), the list's
          // length otherwise — a shell answering the older shape reports no slot rather than throw.
          cssSlotCount: res.css_slot_count ?? cssSlots.length,
          navigation,
          navigationCount: res.navigation_count ?? navigation.length,
          // Same rule for the broken tally: the sidecar's number when it sent one, else counted off
          // the list it did send — never re-derived over the sidecar's own answer.
          navigationBrokenCount:
            res.navigation_broken_count ?? navigation.filter((l) => l.status !== "linked").length,
        });
        setError(null);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setResult(NO_RENDER);
        // THE ONE READING OF A FAILURE (finding `01KYVT6HVA260DTAEREQA4DFJN`). The bridge rejects
        // with a serialized `BridgeError` OBJECT, not an `Error`; this surface used to test
        // `instanceof Error` first and sent the sidecar's readable message through `String(e)`,
        // putting `[object Object]` on the stage — the very "silent blank" the lenient render exists
        // to remove, wearing a message. It is `errorText`'s job now, and only its job.
        setError(errorText(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [projectId, compositionJson, prototypeId, subjectStatesJson]);

  return { ...result, loading, error };
}
