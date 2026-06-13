import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { Sidebar } from "./sidebar";
import type { TerminalRecord } from "./use-terminals";

function row(id: number, cwd: string, order: number): TerminalRecord {
  return {
    id: String(id),
    cwd,
    label: null,
    scrollback: "",
    status: "alive",
    order_index: order,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
  };
}

const noop = vi.fn();

function renderWith(terminals: TerminalRecord[]) {
  return render(
    <Sidebar
      terminals={terminals}
      activeId={terminals[0]?.id ?? null}
      onSelect={noop}
      onClose={noop}
      onCreate={noop}
    />,
  );
}

describe("Sidebar item animation (motion Reorder)", () => {
  it("mounts sortable Reorder items without crashing", () => {
    renderWith([row(1, "/a", 0), row(2, "/b", 1)]);
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
  });

  it("removes a closed item from the list (neighbours slide up via layout)", async () => {
    const { rerender } = renderWith([row(1, "/a", 0), row(2, "/b", 1)]);
    expect(screen.getByText("a")).toBeInTheDocument();
    expect(screen.getByText("b")).toBeInTheDocument();

    // Close the second terminal: the row unmounts and Reorder slides the
    // survivors up via `layout`.
    rerender(
      <Sidebar
        terminals={[row(1, "/a", 0)]}
        activeId={"1"}
        onSelect={noop}
        onClose={noop}
        onCreate={noop}
      />,
    );

    // The closed item ("b") is gone; the survivor ("a") stays. Asserts a removal
    // that drops only the closed row — not a wipe of the whole list.
    await waitFor(() => expect(screen.queryByText("b")).toBeNull());
    expect(screen.getByText("a")).toBeInTheDocument();
  });
});
