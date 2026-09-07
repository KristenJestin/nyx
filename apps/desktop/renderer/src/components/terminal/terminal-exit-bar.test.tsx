import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { TerminalExitBar } from "./terminal-exit-bar";

/**
 * WHAT THE BAR SAYS (jsdom). The browser suite proves it is really there and really clickable in a
 * laid-out pane; what is held here is its COPY — the three readings a user can get out of it, and
 * that the button is the only thing it does.
 */
describe("<TerminalExitBar>", () => {
  it("names the exit code", () => {
    render(<TerminalExitBar exit={{ code: 137 }} onRelaunch={() => {}} />);
    expect(screen.getByRole("status").textContent).toContain("Exited · code 137");
  });

  it("says only that it is finished when the backend read NO code", () => {
    render(<TerminalExitBar exit={{ code: null }} onRelaunch={() => {}} />);
    const text = screen.getByRole("status").textContent ?? "";
    expect(text).toContain("Exited");
    // Never an invented number, and never a bare "code" with nothing after it.
    expect(text).not.toMatch(/code/);
  });

  it("says so when a relaunch already failed, instead of offering the same silent button", () => {
    render(<TerminalExitBar exit={{ code: 1, relaunchFailed: true }} onRelaunch={() => {}} />);
    expect(screen.getByRole("status").textContent).toContain("relaunch failed");
  });

  it("relaunches on click", () => {
    const onRelaunch = vi.fn();
    render(<TerminalExitBar exit={{ code: 0 }} onRelaunch={onRelaunch} />);

    fireEvent.click(screen.getByRole("button", { name: "Relaunch" }));

    expect(onRelaunch).toHaveBeenCalledTimes(1);
  });
});
