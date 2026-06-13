import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { Sidebar } from "./sidebar";
import { displayName } from "./terminal-item";
import type { TerminalRecord } from "./use-terminals";

function row(
  id: number,
  cwd: string,
  order: number,
  label: string | null = null,
): TerminalRecord {
  return {
    id: String(id),
    cwd,
    label,
    scrollback: "",
    status: "alive",
    order_index: order,
    created_at: 0,
    updated_at: 0,
    closed_at: null,
  };
}

const terminals = [
  row(1, "/home/kris/work", 0),
  row(2, "/tmp/scratch", 1),
  row(3, "/srv/api", 2, "api server"),
];

function renderSidebar(overrides: Partial<Parameters<typeof Sidebar>[0]> = {}) {
  const onSelect = vi.fn();
  const onClose = vi.fn();
  const onCreate = vi.fn();
  render(
    <Sidebar
      terminals={terminals}
      activeId={"1"}
      onSelect={onSelect}
      onClose={onClose}
      onCreate={onCreate}
      {...overrides}
    />,
  );
  return { onSelect, onClose, onCreate };
}

describe("displayName (terminal item label)", () => {
  it("uses the explicit label when present", () => {
    expect(displayName(row(3, "/srv/api", 2, "api server"), 2)).toBe(
      "api server",
    );
  });
  it("falls back to the cwd basename", () => {
    expect(displayName(row(1, "/home/kris/work", 0), 0)).toBe("work");
  });
  it("falls back to a numbered name when cwd has no basename", () => {
    expect(displayName(row(1, "/", 0), 0)).toBe("Terminal 1");
  });
});

describe("<Sidebar>", () => {
  it("lists one item per terminal", () => {
    renderSidebar();
    // Three terminals â†’ three list items.
    expect(screen.getAllByRole("listitem")).toHaveLength(3);
    expect(screen.getByText("work")).toBeInTheDocument();
    expect(screen.getByText("scratch")).toBeInTheDocument();
    expect(screen.getByText("api server")).toBeInTheDocument();
  });

  it("marks the active terminal with aria-current", () => {
    renderSidebar({ activeId: "2" });
    const active = screen.getByText("scratch").closest("[aria-current]");
    expect(active).toHaveAttribute("aria-current", "true");
  });

  it("clicking an item selects it", () => {
    const { onSelect } = renderSidebar();
    fireEvent.click(screen.getByText("scratch"));
    expect(onSelect).toHaveBeenCalledWith("2");
  });

  it("the + button creates a terminal", () => {
    const { onCreate } = renderSidebar();
    fireEvent.click(screen.getByRole("button", { name: /new terminal/i }));
    expect(onCreate).toHaveBeenCalledTimes(1);
  });

  it("the per-item x closes that terminal (and does not select it)", () => {
    const { onClose, onSelect } = renderSidebar();
    // The close button for the second item.
    const closeButtons = screen.getAllByRole("button", {
      name: /close terminal/i,
    });
    expect(closeButtons).toHaveLength(3);
    fireEvent.click(closeButtons[1]);
    expect(onClose).toHaveBeenCalledWith("2");
    // Closing must not also trigger a select of that item.
    expect(onSelect).not.toHaveBeenCalled();
  });
});
