import { render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { EngineGraph } from "./engine-graph";

describe("EngineGraph", () => {
  // finding #29 — the React Flow Controls panel follows the theme via design-system TOKENS (no baked
  // light default). The buttons read their colours from these `--xy-controls-*` variables, mapped on
  // the canvas wrapper to token vars (never a literal colour), so they read correctly in dark + light.
  it("themes the Controls onto design-system tokens", () => {
    const { container } = render(
      <EngineGraph nodes={[]} edges={[]} onMarker={vi.fn()} onSelectNode={vi.fn()} />,
    );
    const canvas = container.querySelector('[data-testid="engine-canvas"]') as HTMLElement;
    expect(canvas).toBeTruthy();
    expect(canvas.style.getPropertyValue("--xy-controls-button-background-color")).toBe(
      "var(--card)",
    );
    expect(canvas.style.getPropertyValue("--xy-controls-button-color")).toBe(
      "var(--card-foreground)",
    );
    expect(canvas.style.getPropertyValue("--xy-controls-button-border-color")).toBe(
      "var(--border)",
    );
    // No raw hex leaked into the mapping.
    expect(canvas.getAttribute("style")).not.toMatch(/#[0-9a-fA-F]{3,6}/);
    // The zoom/fit Controls panel is rendered.
    expect(container.querySelector(".react-flow__controls")).toBeTruthy();
  });
});
