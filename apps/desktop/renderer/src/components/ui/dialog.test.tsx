import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Dialog, DialogBackdrop, DialogPopup } from "./dialog";

function Harness({ open }: { open: boolean }) {
  return (
    <Dialog.Root open={open}>
      <Dialog.Portal>
        <DialogBackdrop data-testid="backdrop" />
        <DialogPopup data-testid="popup">
          <Dialog.Title>Hello</Dialog.Title>
        </DialogPopup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

describe("Motion-driven <Dialog> primitives (Base UI + Motion)", () => {
  it("renders the popup content while open, styled with the popover tokens", () => {
    render(<Harness open />);
    // The popup content is present + styled. The actual enter/exit is animated
    // by Motion (jsdom can't time it; the real visible transition is proven by
    // the WebKitGTK release screenshots).
    expect(screen.getByText("Hello")).toBeInTheDocument();
    const popup = screen.getByTestId("popup");
    expect(popup.className).toContain("bg-popover");
  });

  it("backdrop renders with the scrim tokens while open", () => {
    render(<Harness open />);
    const backdrop = screen.getByTestId("backdrop");
    expect(backdrop.className).toContain("bg-background/80");
  });

  it("is absent from the DOM while fully closed (no focus trap / leaked content)", () => {
    // The dialog is NOT kept mounted when closed — it must leave the DOM so it
    // can't trap focus or leak its content. (During a real CLOSE the popup is
    // briefly kept alive by the `actionsRef` deferral while Motion animates the
    // exit, then `onAnimationComplete` calls `actions.unmount()` to drop it —
    // proven visibly by the WebKitGTK release screenshots.)
    render(<Harness open={false} />);
    expect(screen.queryByTestId("popup")).toBeNull();
    expect(screen.queryByText("Hello")).toBeNull();
  });

  it("opens (closed -> open) showing the content and marking it open", () => {
    const { rerender } = render(<Harness open={false} />);
    expect(screen.queryByTestId("popup")).toBeNull();
    rerender(<Harness open />);
    const popup = screen.getByTestId("popup");
    expect(screen.getByText("Hello")).toBeInTheDocument();
    expect(popup.getAttribute("data-open")).toBe("");
  });
});
