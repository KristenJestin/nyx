import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { LifecycleBadge } from "./lifecycle-badge";

describe("LifecycleBadge (post-crash doubt marker)", () => {
  it("renders the `interrupted` state (running -> interrupted) as a labelled warning badge", () => {
    render(<LifecycleBadge state="interrupted" />);
    const badge = screen.getByText("Interrupted");
    expect(badge).toBeTruthy();
    expect(badge.getAttribute("data-lifecycle")).toBe("interrupted");
    // Reuses the shared <Badge> warning variant (design-system token, no raw hex).
    expect(badge.className).toContain("text-warning");
  });

  it("renders the `unknown` state (active -> unknown) with its own distinct label", () => {
    render(<LifecycleBadge state="unknown" />);
    const badge = screen.getByText("Unknown");
    expect(badge).toBeTruthy();
    expect(badge.getAttribute("data-lifecycle")).toBe("unknown");
    // A quieter `muted` doubt so it stays distinguishable from the amber `interrupted`.
    expect(badge.className).toContain("text-muted-foreground");
  });
});
