import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { CommandSourcePanel } from "./command-source-panel";
import type { ManagedCommand } from "./use-commands";

function cmd(over: Partial<ManagedCommand>): ManagedCommand {
  return {
    id: "c1",
    project_id: "p1",
    name: "dev",
    command: "pnpm dev",
    subfolder: null,
    restart_on_startup: false,
    order_index: 0,
    created_at: 0,
    updated_at: 0,
    source_kind: "package_json",
    source_package_json_path: "/repo/package.json",
    source_script_name: "dev",
    source_script_command_snapshot: "vite --host",
    package_manager: "pnpm",
    description: null,
    ...over,
  };
}

function renderPanel(over: Partial<ManagedCommand> = {}, driftValue: string | null = null) {
  const onResync = vi.fn();
  const onUnlink = vi.fn();
  const result = render(
    <CommandSourcePanel
      command={cmd(over)}
      driftValue={driftValue}
      onResync={onResync}
      onUnlink={onUnlink}
    />,
  );
  return { ...result, onResync, onUnlink };
}

/**
 * THE PROVENANCE IS ONE BLOCK, WITH BOTH OF ITS ANSWERS (finding 01M132XFZ5KY8QMJ79HZZSBV3J).
 *
 * It used to be split in two — a read-only strip under the card that was FORBIDDEN to act, and a
 * second rendering of the same three facts inside the edit form with Resync on it while Unlink sat
 * in the form's action row, away from the link it unlinks. The master/detail has one panel per
 * command, so the provenance is one region of it and its two responses hang off the link itself.
 */
describe("<CommandSourcePanel> — the package.json provenance and its two answers", () => {
  it("states WHERE the command comes from: the file and the script", () => {
    renderPanel();
    expect(screen.getByText("/repo/package.json · scripts.dev")).toBeInTheDocument();
  });

  it("in sync: says the imported value IS the one on disk, and offers both actions", () => {
    const { container, onResync, onUnlink } = renderPanel();
    expect(container.querySelector("[data-command-source-drifted]")).toBeNull();
    expect(screen.getByText(/the same as the one on disk/i)).toBeInTheDocument();
    expect(screen.getByText("vite --host")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /resync from package\.json/i }));
    expect(onResync).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: /unlink/i }));
    expect(onUnlink).toHaveBeenCalledTimes(1);
  });

  it("drifted: BOTH values side by side, and the adopt action says what it adopts", () => {
    const { container } = renderPanel({}, "node server.js");
    // The warning register is structural, not a class the eye has to guess at.
    expect(container.querySelector("[data-command-source-drifted]")).not.toBeNull();
    expect(screen.getByText(/the script changed on disk/i)).toBeInTheDocument();
    // At import…
    expect(screen.getByTestId("drift-imported").textContent).toContain("vite --host");
    // …and on disk. The two are readable together, which is the whole point of showing a drift.
    expect(screen.getByTestId("drift-line").textContent).toContain("node server.js");
    expect(screen.getByRole("button", { name: /take the disk version/i })).toBeInTheDocument();
  });

  it("locks both buttons while a source action is in flight", () => {
    render(
      <CommandSourcePanel command={cmd({})} busy="resync" onResync={vi.fn()} onUnlink={vi.fn()} />,
    );
    for (const button of screen.getAllByRole("button")) {
      expect(button).toBeDisabled();
    }
  });

  it("surfaces the backend's own sentence when an action failed", () => {
    render(
      <CommandSourcePanel
        command={cmd({})}
        error="the source is pinned"
        onResync={vi.fn()}
        onUnlink={vi.fn()}
      />,
    );
    expect(screen.getByRole("alert").textContent).toBe("the source is pinned");
  });

  it("renders nothing for a hand-authored (un-sourced) command", () => {
    const { container } = render(
      <CommandSourcePanel
        command={cmd({ source_script_name: null, source_package_json_path: null })}
        onResync={vi.fn()}
        onUnlink={vi.fn()}
      />,
    );
    expect(container.firstChild).toBeNull();
  });
});
