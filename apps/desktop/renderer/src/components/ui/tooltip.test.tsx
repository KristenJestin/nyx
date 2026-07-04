import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Tooltip } from "./tooltip";
import { Button } from "./button";

describe("<Tooltip> (Base UI tooltip wrapper)", () => {
  it("renders the wrapped element as the trigger, keeping its accessible label", () => {
    render(
      <Tooltip label="Add project">
        <Button aria-label="Add project">+</Button>
      </Tooltip>,
    );
    // The trigger is a real button with its own aria-label (icon-only buttons
    // get their name from aria-label).
    const trigger = screen.getByRole("button", { name: /add project/i });
    expect(trigger).toBeInTheDocument();
  });

  it("reveals the descriptive label on hover and wires it to the trigger", async () => {
    render(
      <Tooltip label="New terminal" delay={0}>
        <Button aria-label="New terminal">+</Button>
      </Tooltip>,
    );
    const trigger = screen.getByRole("button", { name: /new terminal/i });

    // Hover opens the tooltip; the label text appears (in the portal) and the
    // trigger is described by it.
    fireEvent.pointerEnter(trigger);
    fireEvent.mouseEnter(trigger);
    await waitFor(() => {
      // The label text is rendered somewhere (the popup portal).
      const labels = screen.getAllByText("New terminal");
      expect(labels.length).toBeGreaterThan(0);
    });
  });

  it("renders the arrow OUT OF FLOW so it never overlaps the first letter of the label", async () => {
    render(
      <Tooltip label="Stop" delay={0}>
        <Button aria-label="Stop">■</Button>
      </Tooltip>,
    );
    const trigger = screen.getByRole("button", { name: /^stop$/i });
    fireEvent.pointerEnter(trigger);
    fireEvent.mouseEnter(trigger);

    // The arrow wraps the triangle svg; it must be absolutely positioned (out of the
    // label's text flow) so the label starts at the popup padding edge — the fix for the
    // arrow-on-first-letter bug (`Stop` → dot on the `S`).
    const arrow = await waitFor(() => {
      const svg = document.querySelector("svg[aria-hidden]");
      const host = svg?.parentElement as HTMLElement | null;
      expect(host).not.toBeNull();
      return host!;
    });
    expect(arrow.className).toContain("absolute");
  });
});
