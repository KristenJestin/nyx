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

describe("<TerminalStateBadge> (terminal run-state, persisted-unread model)", () => {
  it("renders NO badge for idle (nothing to notify)", () => {
    const { container } = render(<TerminalStateBadge state="idle" unread />);
    expect(container.firstChild).toBeNull();
  });

  it.each(["success", "error"] as ExecState[])(
    "renders the %s badge while UNREAD (the persisted flag drives visibility)",
    (state) => {
      render(<TerminalStateBadge state={state} unread />);
      const badge = screen.getByRole("status", {
        name: new RegExp(`terminal status: ${state}`, "i"),
      });
      expect(badge).toHaveAttribute("data-state", state);
    },
  );

  it("HIDES the settled badge once READ — even when the terminal is INACTIVE again (user story #3)", () => {
    for (const state of ["success", "error"] as ExecState[]) {
      // unread=false (the user already viewed it) + active=false (re-deselected):
      // the badge must NOT re-appear — visibility is driven by `unread`, not `active`.
      const { container, unmount } = render(
        <TerminalStateBadge state={state} unread={false} active={false} />,
      );
      expect(container.firstChild).toBeNull();
      unmount();
    }
  });

  it("KEEPS the running (blue, pulsing) badge regardless of unread/active (live state)", () => {
    // Running is a live state, never gated by the unread flag — shows even active+read.
    render(<TerminalStateBadge state="running" unread={false} active />);
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
    // success/error need `unread` to show; running shows regardless.
    for (const state of ["running", "success", "error"] as ExecState[]) {
      const { unmount } = render(<TerminalStateBadge state={state} unread />);
      const badge = screen.getByRole("status");
      expect(badge).toHaveClass(tokens[state]);
      unmount();
    }
  });
});
