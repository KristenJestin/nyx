import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { Menu, MenuItem, MenuSeparator } from "./menu";
import { Button } from "./button";

/**
 * Unit coverage for the Motion-driven `<Menu>` wrapper (Base UI + Motion),
 * mirroring `tooltip.test.tsx` / `dialog.test.tsx` conventions. The real visible
 * enter/exit is animated by Motion (jsdom can't time it; the WebKitGTK release
 * screenshots prove the transition). Here we assert the WIRING: the trigger is a
 * real labelled button with menu semantics, opening reveals the items, an item's
 * action fires and closes the menu, keyboard navigation highlights items, and the
 * tooltip variant keeps the SAME button as the menu trigger.
 */

/** A small menu like the project kebab in `project-item.tsx`. */
function Harness({
  onRename = () => {},
  onDelete = () => {},
  tooltip,
}: {
  onRename?: () => void;
  onDelete?: () => void;
  tooltip?: string;
}) {
  return (
    <Menu
      tooltip={tooltip}
      trigger={
        <Button aria-label="Project actions" size="icon-xs">
          {/* icon-only kebab */}⋮
        </Button>
      }
    >
      <MenuItem onClick={onRename}>Rename</MenuItem>
      <MenuSeparator />
      <MenuItem destructive onClick={onDelete}>
        Delete
      </MenuItem>
    </Menu>
  );
}

describe("<Menu> (Base UI menu wrapper, Motion-animated)", () => {
  it("renders the wrapped element as the trigger, keeping its label + menu wiring", () => {
    render(<Harness />);
    // The trigger is a real button with its own aria-label (icon-only buttons
    // get their accessible name from aria-label) AND the menu semantics Base UI
    // adds (aria-haspopup). It starts closed (no menu in the DOM yet).
    const trigger = screen.getByRole("button", { name: /project actions/i });
    expect(trigger).toBeInTheDocument();
    expect(trigger).toHaveAttribute("aria-haspopup", "menu");
    // Closed: the popup is portaled only when open, so no menu/items exist yet.
    expect(screen.queryByRole("menu")).toBeNull();
    expect(screen.queryByRole("menuitem", { name: "Rename" })).toBeNull();
  });

  it("opens on trigger click, revealing the menu and its items", async () => {
    render(<Harness />);
    const trigger = screen.getByRole("button", { name: /project actions/i });

    fireEvent.click(trigger);

    await waitFor(() => {
      expect(screen.getByRole("menu")).toBeInTheDocument();
    });
    expect(screen.getByRole("menuitem", { name: "Rename" })).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "Delete" })).toBeInTheDocument();
  });

  it("fires the item's onClick and closes the menu on selection", async () => {
    const onRename = vi.fn();
    render(<Harness onRename={onRename} />);
    const trigger = screen.getByRole("button", { name: /project actions/i });

    fireEvent.click(trigger);
    const rename = await screen.findByRole("menuitem", { name: "Rename" });
    fireEvent.click(rename);

    expect(onRename).toHaveBeenCalledTimes(1);
    // Selecting an item closes the menu (default Base UI behaviour) → the popup
    // leaves the DOM.
    await waitFor(() => {
      expect(screen.queryByRole("menu")).toBeNull();
    });
  });

  it("supports keyboard: ArrowDown opens and highlights the first item, Enter activates it", async () => {
    const onRename = vi.fn();
    render(<Harness onRename={onRename} />);
    const trigger = screen.getByRole("button", { name: /project actions/i });

    // ArrowDown from the focused trigger opens the menu and highlights item 1.
    trigger.focus();
    fireEvent.keyDown(trigger, { key: "ArrowDown" });

    const rename = await screen.findByRole("menuitem", { name: "Rename" });
    await waitFor(() => {
      expect(rename).toHaveAttribute("data-highlighted");
    });

    // Enter on the highlighted item activates it (runs its onClick).
    fireEvent.keyDown(rename, { key: "Enter" });
    expect(onRename).toHaveBeenCalledTimes(1);
  });

  it("with a tooltip, the SAME button is both the menu trigger and the tooltip anchor", async () => {
    const onRename = vi.fn();
    render(<Harness tooltip="Project actions" onRename={onRename} />);

    // Exactly one button carries the label, and it still drives the menu (the
    // nested Tooltip.Trigger does not swallow the Menu.Trigger wiring).
    const triggers = screen.getAllByRole("button", { name: /project actions/i });
    expect(triggers).toHaveLength(1);
    const trigger = triggers[0]!;
    expect(trigger).toHaveAttribute("aria-haspopup", "menu");

    fireEvent.click(trigger);
    const rename = await screen.findByRole("menuitem", { name: "Rename" });
    fireEvent.click(rename);
    expect(onRename).toHaveBeenCalledTimes(1);
  });
});
