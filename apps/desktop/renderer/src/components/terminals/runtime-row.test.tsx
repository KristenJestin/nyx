import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

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

describe("RuntimeRow — inline editing (v2.5.6)", () => {
  it("swaps the label for the editor when editing (name no longer rendered as text)", () => {
    render(
      <RuntimeSelectionProvider>
        <RuntimeRow
          selection={{ kind: "terminal", id: "t1" }}
          icon={<span data-testid="row-icon" />}
          label="build"
          state="idle"
          editing
          editor={<input aria-label="rename" defaultValue="build" />}
        />
      </RuntimeSelectionProvider>,
    );
    // The editor is shown in place of the label — the name is no longer a plain text node.
    expect(screen.getByRole("textbox", { name: "rename" })).toBeTruthy();
    expect(screen.queryByText("build")).toBeNull();
  });

  it("fires onLabelDoubleClick on a double-click of the name", () => {
    const onLabelDoubleClick = vi.fn();
    render(
      <RuntimeSelectionProvider>
        <RuntimeRow
          selection={{ kind: "terminal", id: "t1" }}
          icon={<span data-testid="row-icon" />}
          label="build"
          state="idle"
          onLabelDoubleClick={onLabelDoubleClick}
        />
      </RuntimeSelectionProvider>,
    );
    fireEvent.doubleClick(screen.getByText("build"));
    expect(onLabelDoubleClick).toHaveBeenCalledTimes(1);
  });
});
