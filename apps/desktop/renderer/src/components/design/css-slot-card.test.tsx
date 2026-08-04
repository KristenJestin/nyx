import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import designFixtures from "../../../../../../contract/design-fixtures.json";

import { CssSlotCard, cssSlotOfComponent } from "./css-slot-card";
import type { RenderedCssSlot } from "./use-design-render";

/**
 * THE CSS SLOT, SEEN (finding `01KYVVEQZW2Y2VY7ZAPK5RECZS`).
 *
 * The backend of the scoped CSS slot shipped whole — the model, the scope guarantee, the refusal at
 * the write, the coder seam — and NOTHING was displayed. The fourth non-negotiable constraint of a
 * slot is SIGNALLED: visible in review, in the UI. Until it shows, the exception is invisible to the
 * human who re-reads the page, which is precisely the thing the constraint forbids.
 *
 * This suite holds the card to what it must say: the scope, the compiled CSS, the lint, WHOSE
 * exception it is — and, for a stored slot that no longer compiles, a NAMED refusal rather than a
 * disappearance.
 */

const SLOT: RenderedCssSlot = {
  scope_id: "c-spinner-8f2a",
  component: "Spinner",
  scope: '[data-nyx-css~="c-spinner-8f2a"]',
  reason: "a custom pulse keyframe — no Tailwind utility expresses a keyframe",
  features: ["keyframes"],
  bytes: 118,
  css: '[data-nyx-css~="c-spinner-8f2a"] .dot{animation:c-spinner-8f2a-pulse 1.4s infinite}',
  refused: null,
  detail: null,
};

describe("<CssSlotCard> — a slot that applies", () => {
  it("shows the scope, the compiled CSS and the reason it exists", () => {
    render(<CssSlotCard slot={SLOT} />);
    expect(screen.getByTestId("css-slot-scope")).toHaveTextContent(
      '[data-nyx-css~="c-spinner-8f2a"]',
    );
    // The COMPILED css — selectors already anchored — because that is what the frame applied and
    // what the coder must reproduce, not what was authored.
    expect(screen.getByTestId("css-slot-code")).toHaveTextContent("c-spinner-8f2a-pulse");
    expect(screen.getByTestId("css-slot-reason")).toHaveTextContent(
      "no Tailwind utility expresses a keyframe",
    );
    expect(screen.getByTestId("css-slot-c-spinner-8f2a")).toHaveAttribute("data-refused", "false");
  });

  /**
   * THE OWNERSHIP, SAID OUT LOUD. The slot belongs to the COMPONENT; a node only points at it. A
   * card that let the reader believe they are looking at an exception of THIS placement would invite
   * exactly the edit that cannot exist — there is one slot for thirty placements.
   */
  it("names the component as the owner and denies the placement owns it", () => {
    render(<CssSlotCard slot={SLOT} />);
    const card = screen.getByTestId("css-slot-c-spinner-8f2a");
    expect(card).toHaveTextContent("It belongs to the component Spinner, not to this placement");
    expect(card).toHaveTextContent("every placement of Spinner carries it");
    // …and it offers NO control: the renderer neither compiles nor writes a slot.
    expect(card.querySelectorAll("button, input, textarea, select")).toHaveLength(0);
  });

  /** The lint ticks the four allowed constructs and MARKS the ones this slot really exercises. */
  it("ticks the four allowed constructs, marking the ones used", () => {
    render(<CssSlotCard slot={{ ...SLOT, features: ["keyframes", "has"] }} />);
    for (const feature of designFixtures.css_slot.features) {
      // The CODE addresses the chip; the WORDS on it are the fixture's (finding
      // `01KZ3JTWB82DCG0C6JT06T4CAF`: asserting the chip merely EXISTS let a rename through).
      expect(screen.getByTestId(`css-slot-feature-${feature.code}`)).toHaveTextContent(
        feature.short,
      );
    }
    expect(screen.getByTestId("css-slot-feature-keyframes")).toHaveAttribute("data-used", "true");
    expect(screen.getByTestId("css-slot-feature-has")).toHaveAttribute("data-used", "true");
    expect(screen.getByTestId("css-slot-feature-container")).toHaveAttribute("data-used", "false");
    expect(screen.getByTestId("css-slot-lint")).toHaveTextContent("tokens only");
  });

  /** A row stored before the reason became mandatory says so — it never shows a blank line. */
  it("says a missing reason is missing", () => {
    render(<CssSlotCard slot={{ ...SLOT, reason: "  " }} />);
    expect(screen.getByTestId("css-slot-reason")).toHaveTextContent("No reason recorded");
  });
});

describe("<CssSlotCard> — a slot that no longer compiles", () => {
  const REFUSED: RenderedCssSlot = {
    ...SLOT,
    features: [],
    css: "",
    refused: "hardcoded_colour",
    detail:
      "a colour written in the CSS instead of referenced from the token layer — #f43f5e. No exception.",
  };

  /**
   * NAMED, NOT DROPPED. A slot written when the rules were laxer emits nothing; showing nothing
   * would mean the reviewer sees a component that silently lost its exception. So the card stays,
   * carries the refusal's NAME (a code the fixture pins) and the sidecar's own sentence verbatim.
   */
  it("names the refusal, verbatim, and shows no CSS at all", () => {
    render(<CssSlotCard slot={REFUSED} />);
    const refusal = screen.getByTestId("css-slot-refusal");
    // The WORDS are the fixture's, not this file's: the label is the whole of what the reviewer
    // learns about why nothing was applied, and it used to be asserted only as "differs from the
    // code" (finding `01KZ3JTWB82DCG0C6JT06T4CAF`).
    const pinned = designFixtures.css_slot.refusals.find((r) => r.code === REFUSED.refused);
    expect(pinned, "the refusal shown is one the fixture pins").toBeTruthy();
    expect(refusal).toHaveTextContent(`Refused — ${pinned!.short}.`);
    expect(refusal).toHaveTextContent("#f43f5e");
    expect(screen.queryByTestId("css-slot-code")).toBeNull();
    expect(screen.getByTestId("css-slot-c-spinner-8f2a")).toHaveAttribute("data-refused", "true");
    // The scope and the owner are still stated: the exception EXISTS, it just applied nothing.
    expect(screen.getByTestId("css-slot-scope")).toHaveTextContent("c-spinner-8f2a");
  });

  /** A refusal code from a newer sidecar is shown verbatim rather than swallowed into a blank. */
  it("shows an unknown refusal code verbatim", () => {
    render(<CssSlotCard slot={{ ...REFUSED, refused: "a_refusal_from_the_future" }} />);
    expect(screen.getByTestId("css-slot-refusal")).toHaveTextContent("a_refusal_from_the_future");
  });
});

/**
 * THE SAME CARD ON THE COMPONENT'S OWN PAGE (finding `01KYW342EE5DN6F70YB8VSTJR8`, point 7). Only
 * the sentence that PLACES the exception changes: beside a placement the card must deny the
 * placement owns it; on the owner's own page there is no placement to deny, so it states the reach.
 * Everything else — scope, compiled CSS, reason, lint — is the same card, not a second one.
 */
describe("<CssSlotCard> — the `component` surface", () => {
  it("states the reach instead of denying a placement, and keeps everything else", () => {
    render(<CssSlotCard slot={SLOT} surface="component" />);
    const ownership = screen.getByTestId("css-slot-ownership").textContent ?? "";
    expect(ownership).toContain("It belongs to Spinner itself");
    expect(ownership).toContain("every placement of it carries this same exception");
    expect(ownership).not.toContain("not to this placement");
    // The same scope / CSS / lint, unchanged.
    expect(screen.getByTestId("css-slot-scope")).toHaveTextContent(
      '[data-nyx-css~="c-spinner-8f2a"]',
    );
    expect(screen.getByTestId("css-slot-code")).toHaveTextContent("c-spinner-8f2a-pulse");
    expect(screen.getByTestId("css-slot-lint")).toHaveTextContent("tokens only");
  });

  /** The fold's HEAD carries the icon/name/state there, so the card does not repeat them. */
  it("drops its own title bar (the fold's head already carries it)", () => {
    const { unmount } = render(<CssSlotCard slot={SLOT} surface="component" />);
    expect(screen.getByTestId("css-slot-c-spinner-8f2a")).not.toHaveTextContent(
      "CSS slot · scoped",
    );
    unmount();
    render(<CssSlotCard slot={SLOT} />);
    expect(screen.getByTestId("css-slot-c-spinner-8f2a")).toHaveTextContent("CSS slot · scoped");
  });

  /** A refusal stays a refusal on either surface — it is never softened by the wording swap. */
  it("still names a refusal", () => {
    render(
      <CssSlotCard
        slot={{ ...SLOT, css: "", features: [], refused: "important", detail: "`!important`." }}
        surface="component"
      />,
    );
    expect(screen.getByTestId("css-slot-c-spinner-8f2a")).toHaveAttribute("data-refused", "true");
    expect(screen.getByTestId("css-slot-refusal")).toHaveTextContent("Refused — !important.");
  });
});

describe("cssSlotOfComponent", () => {
  /** A node reaches its slot the way `ElectedNode.css_scope` does: through its COMPONENT. */
  it("resolves a component's slot, and nothing for a component that carries none", () => {
    expect(cssSlotOfComponent([SLOT], "Spinner")).toBe(SLOT);
    expect(cssSlotOfComponent([SLOT], "Button")).toBeNull();
    expect(cssSlotOfComponent([SLOT], null)).toBeNull();
    expect(cssSlotOfComponent([], "Spinner")).toBeNull();
  });
});
