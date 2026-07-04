import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { RuntimeRow } from "./runtime-row";
import { RuntimeSelectionProvider } from "./runtime-selection";

function renderRow(state: Parameters<typeof RuntimeRow>[0]["state"]) {
  return render(
    <RuntimeSelectionProvider>
      <RuntimeRow
        selection={{ kind: "terminal", id: "t1" }}
        icon={<span data-testid="row-icon" />}
        label="build"
        state={state}
      />
    </RuntimeSelectionProvider>,
  );
}

describe("RuntimeRow", () => {
  it("renders the name (label) and the lead glyph", () => {
    renderRow("running");
    expect(screen.getByText("build")).toBeTruthy();
    expect(screen.getByTestId("row-icon")).toBeTruthy();
  });

  it("renders the run-state dot for a significant state", () => {
    const { container } = renderRow("running");
    expect(container.querySelector("[data-state]")).not.toBeNull();
  });

  it("renders NO dot on an idle (grey/empty) row", () => {
    const { container } = renderRow("idle");
    // A quiet state paints no dot at all — the shared `isQuietState` rule (finding: hide
    // the dot in grey/empty). The row keeps its name + glyph.
    expect(container.querySelector("[data-state]")).toBeNull();
    expect(screen.getByText("build")).toBeTruthy();
  });
});
