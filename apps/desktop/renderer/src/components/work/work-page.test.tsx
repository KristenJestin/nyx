import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { WorkPage } from "./work-page";

describe("WorkPage", () => {
  it("renders a clean routed stub (title + description)", () => {
    render(<WorkPage />);
    expect(screen.getByRole("heading", { name: "Work" })).toBeTruthy();
    expect(screen.getByText(/routed from the navigation/)).toBeTruthy();
  });

  it("uses design tokens only (no hardcoded colour)", () => {
    const { container } = render(<WorkPage />);
    expect(container.innerHTML).not.toMatch(/#[0-9a-fA-F]{3,6}\b|rgba?\(/);
  });
});
