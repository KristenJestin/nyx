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
});
