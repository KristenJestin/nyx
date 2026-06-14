import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { StatusDot, TerminalStateBadge } from "./run-state";
import type { ExecState } from "./use-terminals";

const ALL_STATES: ExecState[] = ["idle", "running", "success", "error"];

describe("<StatusDot> (command run-state, all 4 states)", () => {
  it.each(ALL_STATES)("renders the %s state with the right token color", (state) => {
    render(<StatusDot state={state} />);
    const dot = screen.getByRole("status", { name: new RegExp(`status: ${state}`, "i") });
    expect(dot).toHaveAttribute("data-state", state);
  });

  it("colors each state from a design-system token (no raw colors)", () => {
    const tokens: Record<ExecState, string> = {
      idle: "bg-muted-foreground/50",
      running: "bg-info",
      success: "bg-success",
      error: "bg-destructive",
    };
    for (const state of ALL_STATES) {
      const { unmount } = render(<StatusDot state={state} />);
      expect(screen.getByRole("status")).toHaveClass(tokens[state]);
      unmount();
    }
  });

  it("pulses ONLY in the running state", () => {
    const { rerender } = render(<StatusDot state="running" />);
    expect(screen.getByRole("status").className).toContain("animate-pulse");
    rerender(<StatusDot state="success" />);
    expect(screen.getByRole("status").className).not.toContain("animate-pulse");
  });
});

describe("<TerminalStateBadge> (terminal run-state, unread model)", () => {
  it("renders NO badge for idle (nothing to notify)", () => {
    const { container } = render(<TerminalStateBadge state="idle" />);
    expect(container.firstChild).toBeNull();
  });

  it.each(["running", "success", "error"] as ExecState[])(
    "renders the %s badge on a NON-active terminal (unread)",
    (state) => {
      render(<TerminalStateBadge state={state} active={false} />);
      const badge = screen.getByRole("status", {
        name: new RegExp(`terminal status: ${state}`, "i"),
      });
      expect(badge).toHaveAttribute("data-state", state);
    },
  );

  it("CLEARS the badge on an ACTIVE terminal for settled states (read)", () => {
    for (const state of ["success", "error"] as ExecState[]) {
      const { container, unmount } = render(<TerminalStateBadge state={state} active />);
      // Selecting/viewing the terminal marks it read → no badge.
      expect(container.firstChild).toBeNull();
      unmount();
    }
  });

  it("KEEPS the running (blue, pulsing) badge even when active (still-running signal)", () => {
    render(<TerminalStateBadge state="running" active />);
    const badge = screen.getByRole("status", { name: /terminal status: running/i });
    expect(badge.className).toContain("bg-info");
    expect(badge.className).toContain("animate-pulse");
  });

  it("badge colors come from tokens (the appear pop is Motion-driven)", () => {
    const tokens: Record<string, string> = {
      running: "bg-info",
      success: "bg-success",
      error: "bg-destructive",
    };
    for (const state of ["running", "success", "error"] as ExecState[]) {
      const { unmount } = render(<TerminalStateBadge state={state} active={false} />);
      const badge = screen.getByRole("status");
      expect(badge).toHaveClass(tokens[state]);
      unmount();
    }
  });
});
