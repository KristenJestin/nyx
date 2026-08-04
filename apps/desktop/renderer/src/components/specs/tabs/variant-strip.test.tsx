import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { PrototypeVariant } from "../proto-types";
import { VariantStrip } from "./variant-strip";

/**
 * DONE-CRITERION (finding `01KYT44FS853KEJE58HQNQPKCD`) — ONE CAN SEE THE OPTIONS BEFORE CHOOSING.
 *
 * The screen only let a variant be RETAINED; there was no way to display one, and nothing said which
 * one was on screen. These pin the arbitrations depot's workspace already rendered: a pill per
 * variant with `aria-pressed`, MAIN / RETAINED legible even on the active fill, a constant pill
 * height, the decision lodged with the variants, and no switch at all on a single-variant page.
 */

function variant(over: Partial<PrototypeVariant> & { id: string }): PrototypeVariant {
  return {
    page_version_id: "pv1",
    label: "A",
    title: "Variant A",
    composition_json: "{}",
    states_json: null,
    is_main: false,
    position: 0,
    created_at: 0,
    ...over,
  };
}

const THREE = [
  variant({ id: "va", label: "A", title: "Card-wrapped", is_main: true }),
  variant({ id: "vb", label: "B", title: "Bare block", position: 1 }),
  variant({ id: "vc", label: "C", title: "Two actions", position: 2 }),
];

function renderStrip(over: Partial<Parameters<typeof VariantStrip>[0]> = {}) {
  const props = {
    variants: THREE,
    selectedVariantId: "va",
    onSelect: vi.fn(),
    chosenVariantId: null,
    effectiveVariantId: null,
    canElect: true,
    pending: false,
    onElect: vi.fn(),
    onClear: vi.fn(),
    notice: null,
    onDismissNotice: vi.fn(),
    ...over,
  };
  render(<VariantStrip {...props} />);
  return props;
}

describe("<VariantStrip>", () => {
  it("offers ONE pill per variant and marks the displayed one with aria-pressed", () => {
    renderStrip({ selectedVariantId: "vb" });
    for (const v of THREE) expect(screen.getByTestId(`proto-variant-${v.id}`)).toBeTruthy();
    expect(screen.getByTestId("proto-variant-vb").getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByTestId("proto-variant-va").getAttribute("aria-pressed")).toBe("false");
    expect(screen.getByTestId("proto-variant-vc").getAttribute("aria-pressed")).toBe("false");
  });

  it("asks its host to DISPLAY another variant when a pill is clicked (the compare gesture)", () => {
    const props = renderStrip();
    fireEvent.click(screen.getByTestId("proto-variant-vc"));
    expect(props.onSelect).toHaveBeenCalledWith("vc");
    // Selecting is NOT electing: comparing must never write a decision.
    expect(props.onElect).not.toHaveBeenCalled();
  });

  it("keeps MAIN and RETAINED readable ON the active pill, in a style made for its fill", () => {
    renderStrip({ selectedVariantId: "va", chosenVariantId: "va" });
    const main = screen.getByTestId("proto-variant-reco-va");
    const retained = screen.getByTestId("proto-variant-retenu-va");
    // Present at all — and rendered in the on-primary chip, not the washed-out neutral/success one.
    expect(main.textContent).toContain("MAIN");
    expect(retained.textContent).toContain("RETAINED");
    expect(main.hasAttribute("data-on-primary")).toBe(true);
    expect(retained.hasAttribute("data-on-primary")).toBe(true);
    expect(main.className).toContain("text-primary-foreground");
  });

  it("keeps the badges in their usual look OFF the active pill", () => {
    renderStrip({ selectedVariantId: "vb", chosenVariantId: "va" });
    expect(screen.getByTestId("proto-variant-reco-va").hasAttribute("data-on-primary")).toBe(false);
    expect(screen.getByTestId("proto-variant-retenu-va").hasAttribute("data-on-primary")).toBe(
      false,
    );
  });

  it("gives every pill the SAME height whether or not it carries a badge", () => {
    renderStrip({ selectedVariantId: "vb", chosenVariantId: "vc" });
    // `h-7` is the constant; `leading-none` on the badges is what stops one adding vertical lead.
    for (const v of THREE) {
      expect(screen.getByTestId(`proto-variant-${v.id}`).className).toContain("h-7");
    }
    expect(screen.getByTestId("proto-variant-reco-va").className).toContain("leading-none");
  });

  it("hides the switch on a single-variant page and keeps only the label", () => {
    renderStrip({ variants: [THREE[0]!], selectedVariantId: "va" });
    expect(screen.queryByTestId("proto-variant-va")).toBeNull();
    const sole = screen.getByTestId("proto-variant-sole-va");
    expect(sole.textContent).toContain("A");
    expect(sole.textContent).toContain("Card-wrapped");
  });

  it("lodges the DECISION in the strip and elects the variant ON SCREEN", () => {
    const props = renderStrip({ selectedVariantId: "vc" });
    fireEvent.click(screen.getByTestId("proto-variant-elect-vc"));
    expect(props.onElect).toHaveBeenCalledWith("vc");
  });

  it("offers to CLEAR the decision when the displayed variant is the retained one", () => {
    const props = renderStrip({ selectedVariantId: "va", chosenVariantId: "va" });
    expect(screen.queryByTestId("proto-variant-elect-va")).toBeNull();
    fireEvent.click(screen.getByTestId("proto-variant-clear-va"));
    expect(props.onClear).toHaveBeenCalled();
  });

  it("lets a retained page be re-decided from the variant one is looking at", () => {
    const props = renderStrip({ selectedVariantId: "vb", chosenVariantId: "va" });
    fireEvent.click(screen.getByTestId("proto-variant-elect-vb"));
    expect(props.onElect).toHaveBeenCalledWith("vb");
  });

  it("marks a mono-variant page retained BY DEFAULT — a state, not a button", () => {
    renderStrip({
      variants: [THREE[0]!],
      selectedVariantId: "va",
      chosenVariantId: null,
      effectiveVariantId: "va",
    });
    expect(screen.getByTestId("proto-variant-office-va").textContent).toContain("by default");
    expect(screen.queryByTestId("proto-variant-elect-va")).toBeNull();
    expect(screen.queryByTestId("proto-variant-clear-va")).toBeNull();
  });

  it("says why there is no decision to write instead of offering a dead button", () => {
    renderStrip({ canElect: false });
    expect(screen.queryByTestId("proto-variant-elect-va")).toBeNull();
    expect(screen.getByTestId("proto-election-unavailable").textContent).toContain(
      "Not pinned in the current round",
    );
  });

  it("carries the FREEZE orientation and lets it be dismissed", () => {
    const props = renderStrip({
      notice: { kind: "frozen", message: "Election frozen: this round is presented (locked)." },
    });
    const notice = screen.getByTestId("proto-election-notice");
    expect(notice.getAttribute("role")).toBe("status");
    expect(notice.textContent).toContain("Election frozen");
    fireEvent.click(screen.getByLabelText("Dismiss"));
    expect(props.onDismissNotice).toHaveBeenCalled();
  });

  /**
   * THE REFUSAL'S OWN SENTENCE, DISPLAYED (finding `01KYVT6HVA260DTAEREQA4DFJN`). The notice names
   * the gesture that failed; the backend says WHY, and that line used to live only in a toast — i.e.
   * it was gone four seconds later, leaving a generic sentence and no lever.
   */
  it("shows the backend's own reason under the failed gesture", () => {
    renderStrip({
      notice: {
        kind: "error",
        message: "The variant could not be retained.",
        detail:
          "round v1 is not the current round: elect on the current round or roll back onto it",
      },
    });
    const notice = screen.getByTestId("proto-election-notice");
    expect(notice.getAttribute("role")).toBe("alert");
    expect(notice.textContent).toContain("The variant could not be retained.");
    expect(screen.getByTestId("proto-election-notice-detail").textContent).toContain(
      "not the current round",
    );
    expect(notice.textContent).not.toContain("[object Object]");
  });

  it("gates every control while a write is in flight", () => {
    renderStrip({ selectedVariantId: "vb", pending: true });
    expect((screen.getByTestId("proto-variant-elect-vb") as HTMLButtonElement).disabled).toBe(true);
  });

  it("renders nothing at all when the version has no variant", () => {
    const { container } = render(
      <VariantStrip
        variants={[]}
        selectedVariantId={null}
        onSelect={vi.fn()}
        chosenVariantId={null}
        effectiveVariantId={null}
        canElect={false}
        pending={false}
        onElect={vi.fn()}
        onClear={vi.fn()}
        notice={null}
        onDismissNotice={vi.fn()}
      />,
    );
    expect(container.firstChild).toBeNull();
  });
});
