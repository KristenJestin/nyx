import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { KnowledgePage } from "./knowledge-page";

describe("KnowledgePage", () => {
  it("renders a clean routed stub (title + description)", () => {
    render(<KnowledgePage />);
    expect(screen.getByRole("heading", { name: "Knowledge" })).toBeTruthy();
    expect(screen.getByText(/routed from the navigation/)).toBeTruthy();
  });

  it("uses design tokens only (no hardcoded colour)", () => {
    const { container } = render(<KnowledgePage />);
    expect(container.innerHTML).not.toMatch(/#[0-9a-fA-F]{3,6}\b|rgba?\(/);
  });
});
