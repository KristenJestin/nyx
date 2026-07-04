import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { PagePlaceholder } from "./page-placeholder";

describe("PagePlaceholder", () => {
  it("renders the title as a heading and the description", () => {
    render(<PagePlaceholder title="Work" description="Coming soon." />);
    expect(screen.getByRole("heading", { name: "Work" })).toBeTruthy();
    expect(screen.getByText("Coming soon.")).toBeTruthy();
  });

  it("renders an optional icon slot", () => {
    render(
      <PagePlaceholder
        title="Knowledge"
        description="Soon."
        icon={<svg data-testid="ph-icon" />}
      />,
    );
    expect(screen.getByTestId("ph-icon")).toBeTruthy();
  });

  it("uses token classes only (no raw hex / rgb)", () => {
    const { container } = render(<PagePlaceholder title="T" description="D" />);
    expect(container.innerHTML).not.toMatch(/#[0-9a-fA-F]{3,6}\b|rgba?\(/);
  });
});
